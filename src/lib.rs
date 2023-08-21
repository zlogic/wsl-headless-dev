use core::fmt;
use std::error::Error;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::signal;

use clap::Parser;
use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE,
};

use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, TRUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    GetConsoleMode, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
};

const LAUNCH_COMMAND: &'static str = "/usr/sbin/sshd -D -f ~/.ssh/sshd/sshd_config";
const SHUTDOWN_COMMAND: &'static str = "kill $(cat ~/.ssh/sshd.pid); rm ~/.ssh/sshd.pid";
const PREVENT_SLEEP_TIMER: Duration = Duration::from_secs(60);

// CLI
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
/// Run an SSH port redirector and keep Windows awake
pub struct Args {
    /// Keep display on
    #[clap(long)]
    pub display: bool,
    /// Start command
    #[clap(default_value = LAUNCH_COMMAND)]
    pub launch_command: String,
    /// Shutdown command
    #[clap(default_value = SHUTDOWN_COMMAND)]
    pub shutdown_command: String,
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
        let listener = TcpListener::bind(self.listen_address).await?;
        print!(
            "Opened listener on \x1b[1m{}\x1b[0m\r\n",
            self.listen_address
        );

        let mut command = WslRunner::launch_command(self.launch_command)?;
        let mut stdin = command.stdin.take();
        tokio::spawn(WslRunner::redirect_stream(command.stdout.take()));
        tokio::spawn(WslRunner::redirect_stream(command.stderr.take()));
        let mut interval = tokio::time::interval(PREVENT_SLEEP_TIMER);
        loop {
            tokio::select! {
                _ = signal::ctrl_c() => {
                    break;
                }
                val = command.wait() => {
                    match val {
                        Ok(exit_status) => print!("Command exited: \x1b[1m{}\x1b[0m\r\n", exit_status),
                        Err(err) => print!("Command failed with \x1b[1m{}\x1b[0m error\r\n",err),
                    }
                    // TODO: improve error handling here.
                    command = WslRunner::launch_command(self.launch_command)?;
                    stdin = command.stdin.take();
                    tokio::spawn(WslRunner::redirect_stream(command.stdout.take()));
                    tokio::spawn(WslRunner::redirect_stream(command.stderr.take()));
                }
                val = listener.accept() =>{
                    if let Ok((ingress, addr)) = val {
                        print!("Received connection from \x1b[1m{}\x1b[0m\r\n", addr);
                        let target_address = self.target_address.to_string();
                        tokio::spawn(WslRunner::handle_socket(ingress, addr, target_address));
                    }
                }
                _ = interval.tick() =>{
                    prevent_sleep()
                }
            }
        }
        drop(stdin);

        let mut shutdown_command = WslRunner::launch_command(self.shutdown_command)?;
        shutdown_command.wait().await?;

        command.wait().await?;
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
        while let Some(line) = lines.next_line().await? {
            print!("Command output: \x1b[0;35m{}\x1b[0;39m\r\n", line)
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

    async fn handle_socket(
        mut ingress: TcpStream,
        addr: SocketAddr,
        target_address: String,
    ) -> Result<(), std::io::Error> {
        let mut egress = TcpStream::connect(target_address).await.unwrap();
        match tokio::io::copy_bidirectional(&mut ingress, &mut egress).await {
            Ok((to_egress, to_ingress)) => {
                print!(
                    "Connection with \x1b[1m{}\x1b[0m ended gracefully ({} sent, {} bytes received)\r\n",
                    addr, to_ingress, to_egress
                );
                Ok(())
            }
            Err(err) => {
                print!(
                    "Error while proxying from \x1b[1m{}\x1b[0m: \x1b[0;31m{}\x1b[0;39m\r\n",
                    addr, err
                );
                Err(err)
            }
        }
    }

    fn run(&self) -> Result<(), std::io::Error> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.wait_termination())?;
        rt.shutdown_timeout(Duration::from_secs(60));
        Ok(())
    }
}

fn enable_vt100_mode() -> Result<(), Box<dyn Error>> {
    unsafe {
        let console_handle = CreateFileW(
            windows::w!("CONOUT$"),
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
        if GetConsoleMode(console_handle, &mut console_mode) != TRUE {
            return Err(ConsoleError::new("Cannot get console mode").into());
        };
        console_mode |= ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if SetConsoleMode(console_handle, console_mode) != TRUE {
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

    let launch_command = &args.launch_command.as_str();
    let shutdown_command = &args.shutdown_command.as_str();
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
