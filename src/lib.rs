use std::error::Error;
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use std::{env, fmt};

use async_executor::Executor;
use async_io::Timer;
use async_net::{TcpListener, TcpStream};
use async_process::{Child, Command};
use futures_lite::io::{AsyncBufReadExt, AsyncRead, BufReader};
use futures_lite::stream::StreamExt;
use futures_lite::{future, io};

use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE,
};

use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    GetConsoleMode, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
};

const LAUNCH_COMMAND: &str = "/usr/sbin/sshd -D -f ~/.ssh/sshd/sshd_config";
const SHUTDOWN_COMMAND: &str = "kill $(cat ~/.ssh/sshd.pid); rm ~/.ssh/sshd.pid";
const PREVENT_SLEEP_TIMER: Duration = Duration::from_secs(60);

pub struct Args {
    pub launch_command: String,
    pub shutdown_command: String,
}

impl Args {
    pub fn parse() -> Args {
        let mut launch_command: Option<String> = None;
        let mut shutdown_command: Option<String> = None;

        for arg in env::args() {
            let (name, value) = if let Some(arg) = arg.split_once('=') {
                arg
            } else {
                continue;
            };
            if name == "--launch-command" {
                launch_command = Some(value.to_string());
            } else if name == "--shutdown-command" {
                shutdown_command = Some(value.to_string());
            }
        }

        Args {
            launch_command: launch_command.unwrap_or(LAUNCH_COMMAND.to_string()),
            shutdown_command: shutdown_command.unwrap_or(SHUTDOWN_COMMAND.to_string()),
        }
    }
}

struct WslRunner<'a> {
    listen_address: &'a str,
    target_address: &'a str,
    launch_command: &'a str,
    shutdown_command: &'a str,
}

impl WslRunner<'_> {
    fn new<'a>(launch_command: &'a str, shutdown_command: &'a str) -> WslRunner<'a> {
        WslRunner {
            listen_address: "0.0.0.0:22",
            target_address: "localhost:2022",
            launch_command,
            shutdown_command,
        }
    }

    async fn wait_termination(&self) -> Result<(), std::io::Error> {
        let ex = Arc::new(Executor::new());
        print!(
            "Opened listener on \x1b[1m{}\x1b[0m\r\n",
            self.listen_address
        );

        let command_ex = ex.clone();
        let command_task = ex.spawn(async move {
            loop {
                let mut command = match WslRunner::launch_command(self.launch_command) {
                    Ok(command) => command,
                    Err(err) => {
                        print!("Command failed with \x1b[1m{}\x1b[0m error\r\n", err);
                        break;
                    }
                };
                let redirect_stdin =
                    command_ex.spawn(WslRunner::redirect_stream(command.stdout.take()));
                let redirect_stderr =
                    command_ex.spawn(WslRunner::redirect_stream(command.stderr.take()));
                match command.status().await {
                    Ok(exit_status) => print!("Command exited: \x1b[1m{}\x1b[0m\r\n", exit_status),
                    Err(err) => print!("Command failed with \x1b[1m{}\x1b[0m error\r\n", err),
                }
                let _ = redirect_stdin.cancel().await;
                let _ = redirect_stderr.cancel().await;
            }
        });
        let prevent_sleep_task = ex.spawn(async {
            let mut interval = Timer::interval(PREVENT_SLEEP_TIMER);
            loop {
                prevent_sleep();
                interval.next().await;
            }
        });

        let listener = TcpListener::bind(self.listen_address).await?;
        let client_ex = ex.clone();
        let listen_socket_task = ex.spawn(async move {
            loop {
                if let Ok((ingress, addr)) = listener.accept().await {
                    print!("Received connection from \x1b[1m{}\x1b[0m\r\n", addr);
                    let target_address = self.target_address.to_string();
                    let egress = TcpStream::connect(target_address).await.unwrap();
                    WslRunner::handle_socket(&client_ex, ingress, egress, addr);
                }
            }
        });

        {
            let (s, ctrl_c) = async_channel::bounded(100);
            let handle = move || {
                s.try_send(()).ok();
            };
            ctrlc::set_handler(handle).unwrap();
            future::block_on(ex.run(async {
                let _ = ctrl_c.recv().await;
            }));
        }

        let mut shutdown_command = WslRunner::launch_command(self.shutdown_command)?;
        shutdown_command.status().await?;

        command_task.cancel().await;
        prevent_sleep_task.cancel().await;
        listen_socket_task.cancel().await;
        Ok(())
    }

    async fn redirect_stream<R: AsyncRead + Unpin>(
        stream: Option<R>,
    ) -> Result<(), std::io::Error> {
        let stream = if let Some(stream) = stream {
            stream
        } else {
            return Ok(());
        };
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();
        while let Some(line) = lines.next().await {
            match line {
                Ok(line) => print!("Command output: \x1b[0;35m{}\x1b[0;39m\r\n", line),
                Err(err) => print!("Failed to read output: \x1b[1m{}\x1b[0m\r\n", err),
            }
        }
        Ok(())
    }

    fn launch_command(cmd: &str) -> Result<Child, std::io::Error> {
        Command::new("wsl")
            .arg("bash")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped())
            .spawn()
    }

    fn handle_socket(
        ex: &async_executor::Executor,
        ingress: TcpStream,
        egress: TcpStream,
        addr: SocketAddr,
    ) {
        ex.spawn(async move {
            let (mut ingress_read, mut ingress_write) = io::split(ingress);
            let (mut egress_read, mut egress_write) = io::split(egress);
            let i_to_e = io::copy(&mut ingress_read, &mut egress_write);
            let e_to_i = io::copy(&mut egress_read, &mut ingress_write);
            match future::zip(i_to_e, e_to_i).await {
                (Ok(to_egress), Ok(to_ingress)) => {
                    print!(
                        "Connection with \x1b[1m{}\x1b[0m ended gracefully ({} sent, {} bytes received)\r\n",
                        addr, to_ingress, to_egress
                    );
                }
                (Ok(_), Err(err)) | (Err(err), Ok(_)) => {
                    print!(
                        "Error while proxying from \x1b[1m{}\x1b[0m: \x1b[0;31m{}\x1b[0;39m\r\n",
                        addr, err
                    );
                }
                (Err(err1), Err(err2)) => {
                    print!(
                        "Error while proxying from \x1b[1m{}\x1b[0m: \x1b[0;31m{}; {}\x1b[0;39m\r\n",
                        addr, err1, err2
                    );
                }
            };
        })
        .detach();
    }

    fn run(&self) -> Result<(), std::io::Error> {
        async_io::block_on(self.wait_termination())
    }
}

fn enable_vt100_mode() -> Result<(), Box<dyn Error>> {
    unsafe {
        let console_handle = CreateFileW(
            windows::core::w!("CONOUT$"),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?;
        if console_handle == INVALID_HANDLE_VALUE {
            return Err(ConsoleError::new("Cannot access console window").into());
        }
        let mut console_mode = CONSOLE_MODE(0);
        if GetConsoleMode(console_handle, &mut console_mode).is_err() {
            return Err(ConsoleError::new("Cannot get console mode").into());
        };
        console_mode |= ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if SetConsoleMode(console_handle, console_mode).is_err() {
            Err(ConsoleError::new("Failed to set VT100 console mode").into())
        } else {
            Ok(())
        }
    }
}

fn prevent_sleep() {
    // ES_CONTINUOUS keeps the flag while the thread is running; ES_SYSTEM_REQUIRED prevents system sleep.
    // Add ES_DISPLAY_REQUIRED flag to prevent display from turning off.
    const REQUESTED_ES: EXECUTION_STATE = EXECUTION_STATE(ES_CONTINUOUS.0 | ES_SYSTEM_REQUIRED.0);
    let previous_state = unsafe { SetThreadExecutionState(REQUESTED_ES) };

    if previous_state != REQUESTED_ES {
        print!(
        "Preventing system sleep (changed thread execution state from \x1b[0;31m{:#X}\x1b[0;39;49m to \x1b[0;34m{:#X}\x1b[0;39;49m\r\n", previous_state.0, REQUESTED_ES.0);
    }
}

pub fn run(args: Args) -> Result<(), Box<dyn Error>> {
    enable_vt100_mode()?;

    let launch_command = &args.launch_command;
    let shutdown_command = &args.shutdown_command;
    WslRunner::new(launch_command, shutdown_command)
        .run()
        .map_err(|e| e.into())
}

#[derive(Debug)]
pub struct ConsoleError {
    msg: &'static str,
}

impl ConsoleError {
    fn new(msg: &'static str) -> ConsoleError {
        ConsoleError { msg }
    }
}

impl std::error::Error for ConsoleError {}

impl fmt::Display for ConsoleError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.msg)
    }
}
