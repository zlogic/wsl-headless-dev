use std::error::Error;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;
use std::{env, fmt};

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::runtime::Handle;
use tokio::signal;
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE, SetThreadExecutionState,
};

use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, SetConsoleMode,
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
    listen_addresses: &'a str,
    target_address: &'a str,
    launch_command: &'a str,
    shutdown_command: &'a str,
}

impl WslRunner<'_> {
    fn new<'a>(launch_command: &'a str, shutdown_command: &'a str) -> WslRunner<'a> {
        WslRunner {
            listen_addresses: "0.0.0.0:22 :::22",
            target_address: "localhost:2022",
            launch_command,
            shutdown_command,
        }
    }

    fn run(self) -> Result<(), std::io::Error> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()?;

        let launch_command = self.launch_command.to_owned();
        let command_task = rt.spawn(async move {
            loop {
                let mut command = match Self::launch_command(&launch_command) {
                    Ok(command) => command,
                    Err(err) => {
                        print!("Command failed with \x1b[1m{err}\x1b[0m error\r\n");
                        break;
                    }
                };
                let rt = Handle::current();
                let redirect_stdout = rt.spawn(WslRunner::redirect_stream(command.stdout.take()));
                let redirect_stderr = rt.spawn(WslRunner::redirect_stream(command.stderr.take()));
                match command.wait().await {
                    Ok(exit_status) => print!("Command exited: \x1b[1m{exit_status}\x1b[0m\r\n"),
                    Err(err) => print!("Command failed with \x1b[1m{err}\x1b[0m error\r\n"),
                }
                redirect_stdout.abort();
                redirect_stderr.abort();
            }
        });
        let prevent_sleep_task = rt.spawn(async {
            let mut interval = tokio::time::interval(PREVENT_SLEEP_TIMER);
            loop {
                prevent_sleep();
                interval.tick().await;
            }
        });

        let listen_socket_tasks = self
            .listen_addresses
            .split_whitespace()
            .map(|listen_address| {
                let target_address = self.target_address.to_string();
                let listen_address = listen_address.to_owned();
                rt.spawn(async move {
                    let listener = match TcpListener::bind(&listen_address).await {
                        Ok(listener) => listener,
                        Err(err) => {
                            print!(
                                "Failed to open listener on {listen_address} with \x1b[1m{err}\x1b[0m error\r\n"
                            );
                            return;
                        }
                    };

                    print!("Opened listener on \x1b[1m{}\x1b[0m\r\n", listen_address);
                    loop {
                        if let Ok((ingress, addr)) = listener.accept().await {
                            print!("Received connection from \x1b[1m{addr}\x1b[0m\r\n");
                            let target_address = target_address.to_string();
                            let egress = TcpStream::connect(target_address).await.unwrap();
                            let rt = Handle::current();
                            rt.spawn(WslRunner::handle_socket(ingress, egress, addr));
                        }
                    }
                })
            })
            .collect::<Vec<_>>();

        rt.block_on(async {
            signal::ctrl_c().await?;

            command_task.abort();

            let mut shutdown_command = Self::launch_command(self.shutdown_command)?;
            shutdown_command.wait().await?;

            prevent_sleep_task.abort();
            listen_socket_tasks
                .iter()
                .for_each(|listen_socket_task| listen_socket_task.abort());
            Ok(())
        })
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
        while let Some(line) = lines.next_line().await? {
            print!("Command output: \x1b[0;35m{line}\x1b[0;39m\r\n");
        }
        Ok(())
    }

    fn launch_command(cmd: &str) -> Result<Child, std::io::Error> {
        Command::new("wsl")
            .arg("bash")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    }

    async fn handle_socket(
        mut ingress: TcpStream,
        mut egress: TcpStream,
        addr: SocketAddr,
    ) -> Result<(), std::io::Error> {
        match tokio::io::copy_bidirectional(&mut ingress, &mut egress).await {
            Ok((to_egress, to_ingress)) => {
                print!(
                    "Connection with \x1b[1m{addr}\x1b[0m ended gracefully ({to_ingress} sent, {to_egress} bytes received)\r\n"
                );
                Ok(())
            }
            Err(err) => {
                print!(
                    "Error while proxying from \x1b[1m{addr}\x1b[0m: \x1b[0;31m{err}\x1b[0;39m\r\n"
                );
                Err(err)
            }
        }
    }
}

fn enable_vt100_mode() -> Result<(), RunnerError> {
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
            return Err("Cannot access console window".into());
        }
        let mut console_mode = CONSOLE_MODE(0);
        if GetConsoleMode(console_handle, &mut console_mode).is_err() {
            return Err("Cannot get console mode".into());
        };
        console_mode |= ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if SetConsoleMode(console_handle, console_mode).is_err() {
            Err("Failed to set VT100 console mode".into())
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
            "Preventing system sleep (changed thread execution state from \x1b[0;31m{:#X}\x1b[0;39;49m to \x1b[0;34m{:#X}\x1b[0;39;49m)\r\n",
            previous_state.0, REQUESTED_ES.0
        );
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
pub enum RunnerError {
    Internal(&'static str),
    Io(std::io::Error),
    Windows(windows::core::Error),
}

impl std::error::Error for RunnerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            Self::Internal(_) => None,
            Self::Io(ref err) => Some(err),
            Self::Windows(ref err) => Some(err),
        }
    }
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Self::Internal(msg) => f.write_str(msg),
            Self::Io(ref err) => err.fmt(f),
            Self::Windows(ref err) => err.fmt(f),
        }
    }
}

impl From<std::io::Error> for RunnerError {
    fn from(err: std::io::Error) -> RunnerError {
        Self::Io(err)
    }
}

impl From<windows::core::Error> for RunnerError {
    fn from(err: windows::core::Error) -> RunnerError {
        Self::Windows(err)
    }
}

impl From<&'static str> for RunnerError {
    fn from(msg: &'static str) -> RunnerError {
        Self::Internal(msg)
    }
}
