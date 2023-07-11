use std::error::Error;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::signal;

use clap::Parser;
use colored::Colorize;
use windows::Win32::System::Power::{
    SetThreadExecutionState,
    // ES_AWAYMODE_REQUIRED,
    ES_CONTINUOUS,
    ES_DISPLAY_REQUIRED,
    ES_SYSTEM_REQUIRED,
    // ES_USER_PRESENT,
    EXECUTION_STATE,
};

// CLI
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
/// Run an SSH port redirector and keep Windows awake
pub struct Args {
    /// Keep display on
    #[clap(long)]
    pub display: bool,
}

// Storing the execution state within a struct ensures that the thread execution state
//   is reset to ES_CONTINUOUS (via the implementation of the Drop trait) when the struct
//   goes out of scope
struct StayAwake(EXECUTION_STATE);

impl StayAwake {
    fn new() -> Self {
        Self(ES_CONTINUOUS)
    }

    fn update_execution_state(&self, next_es: EXECUTION_STATE) -> EXECUTION_STATE {
        unsafe { SetThreadExecutionState(ES_CONTINUOUS | next_es) }
    }
}

impl Drop for StayAwake {
    fn drop(&mut self) {
        let next_es = ES_CONTINUOUS;
        let next_es_label = execution_state_as_string(next_es);
        let prev_es = self.update_execution_state(next_es);
        let prev_es_label = execution_state_as_string(prev_es);
        print!(
            "Reset thread execution state:\r\n    {} ==> {} ({:#X})\r\n      {} ==> {} ({:#X})\r\n",
            String::from("From").red(),
            prev_es_label,
            prev_es.0,
            String::from("To").blue(),
            next_es_label,
            next_es.0
        );
    }
}

// Helper
const ES_CONT_BOR_ES_DISPLAY_BOR_ES_SYSTEM: EXECUTION_STATE =
    EXECUTION_STATE(ES_CONTINUOUS.0 | ES_DISPLAY_REQUIRED.0 | ES_SYSTEM_REQUIRED.0);
const ES_CONT_BOR_ES_SYSTEM: EXECUTION_STATE =
    EXECUTION_STATE(ES_CONTINUOUS.0 | ES_SYSTEM_REQUIRED.0);

fn execution_state_as_string(es: EXECUTION_STATE) -> String {
    match es {
        ES_CONTINUOUS => String::from("ES_CONTINUOUS"),
        ES_DISPLAY_REQUIRED => String::from("ES_DISPLAY_REQUIRED"),
        ES_SYSTEM_REQUIRED => String::from("ES_SYSTEM_REQUIRED"),
        ES_CONT_BOR_ES_DISPLAY_BOR_ES_SYSTEM => {
            String::from("ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED")
        }
        ES_CONT_BOR_ES_SYSTEM => String::from("ES_CONTINUOUS | ES_SYSTEM_REQUIRED"),
        _ => String::from("???"),
    }
}

struct WslRunner {
    listen_address: &'static str,
    target_address: &'static str,
    launch_command: &'static str,
    shutdown_command: &'static str,
}

impl WslRunner {
    fn new() -> WslRunner {
        WslRunner {
            listen_address: "0.0.0.0:22",
            target_address: "localhost:2022",
            launch_command: "/usr/sbin/sshd -D -o Port=2022 -o HostKey=~/.ssh/sshd/ssh_host_rsa_key -o HostKey=~/.ssh/sshd/ssh_host_ecdsa_key -o HostKey=~/.ssh/sshd/ssh_host_ed25519_key -o PidFile=/run/user/$(id -u)/sshd.pid",
            shutdown_command: "kill $(cat /run/user/$(id -u)/sshd.pid)",
        }
    }

    async fn wait_termination(&self) -> Result<(), std::io::Error> {
        let listener = TcpListener::bind(self.listen_address).await?;
        print!(
            "Opened listener on {}\r\n",
            String::from(self.listen_address).bold()
        );

        let mut command = WslRunner::launch_command(self.launch_command)?;
        let mut stdin = command.stdin.take();
        tokio::spawn(WslRunner::redirect_stream(command.stdout.take()));
        tokio::spawn(WslRunner::redirect_stream(command.stderr.take()));
        loop {
            tokio::select! {
                _ = signal::ctrl_c() => {
                    break;
                }
                val = command.wait() => {
                    match val {
                        Ok(exit_status) => print!("Command exited: {}\r\n", exit_status.to_string().green()),
                        Err(err) => print!("Command failed with {} error\r\n",err.to_string().green()),
                    }
                    // TODO: improve error handling here.
                    command = WslRunner::launch_command(self.launch_command)?;
                    stdin = command.stdin.take();
                    tokio::spawn(WslRunner::redirect_stream(command.stdout.take()));
                    tokio::spawn(WslRunner::redirect_stream(command.stderr.take()));
                }
                val = listener.accept() =>{
                    if let Ok((ingress, addr)) = val {
                        print!("Received connection from {}\r\n", addr.to_string().bold());
                        let target_address = self.target_address;
                        tokio::spawn(WslRunner::handle_socket(ingress, addr, target_address));
                    }
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
            print!("Command output: {}\r\n", line.yellow())
        }
        Ok(())
    }

    fn launch_command(cmd: &'static str) -> Result<Child, std::io::Error> {
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
        target_address: &'static str,
    ) -> Result<(), std::io::Error> {
        let mut egress = TcpStream::connect(target_address).await.unwrap();
        match tokio::io::copy_bidirectional(&mut ingress, &mut egress).await {
            Ok((to_egress, to_ingress)) => {
                print!(
                    "Connection with {} ended gracefully ({} bytes from client, {} bytes from server)\r\n",
                    addr,
                    to_egress.to_string().purple(),
                    to_ingress.to_string().blue(),
                );
                Ok(())
            }
            Err(err) => {
                print!(
                    "Error while proxying (addr {}): {}\r\n",
                    addr,
                    err.to_string().red()
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

// Run
pub fn run(args: Args) -> Result<(), Box<dyn Error>> {
    // requested execution state
    let req_es = if args.display {
        print!("Running in {} mode ==> the machine will not go to sleep and the display will remain on\r\n", String::from("Display").green());

        ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED
    } else {
        print!(
            "Running in {} mode ==> the machine will not go to sleep\r\n",
            String::from("System").green()
        );

        ES_SYSTEM_REQUIRED
    };

    // state to set - must combine ES_CONTINUOUS with another state
    let next_es = ES_CONTINUOUS | req_es;
    let next_es_label = execution_state_as_string(next_es);

    // initialize struct
    let sa = StayAwake::new();

    // set thread execution state
    let prev_es = sa.update_execution_state(next_es);
    let prev_es_label = execution_state_as_string(prev_es);

    // print
    print!(
        "Set thread execution state:\r\n    {} ==> {} ({:#X})\r\n      {} ==> {} ({:#X})\r\n",
        String::from("From").purple(),
        prev_es_label,
        prev_es.0,
        String::from("To").cyan(),
        next_es_label,
        next_es.0
    );

    // After exiting main, StayAwake instance is dropped and the thread execution
    //   state is reset to ES_CONTINUOUS

    WslRunner::new().run().map_err(|e| e.into())
}
