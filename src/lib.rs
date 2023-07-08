use std::error::Error;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio::sync::mpsc::{channel, Receiver};

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
/// Keep a Windows machine awake
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
        println!(
            "\nReset thread execution state:\n    {} ==> {} ({:#X})\n      {} ==> {} ({:#X})",
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
}

impl WslRunner {
    fn new() -> WslRunner {
        WslRunner {
            listen_address: "0.0.0.0:2022",
            target_address: "localhost:2022",
        }
    }

    async fn wait_termination(&self) -> Result<(), std::io::Error> {
        let inner_runtime = tokio::runtime::Runtime::new().unwrap();
        let (send, mut recv) = channel::<()>(1);
        self.listen_port().await?;
        signal::ctrl_c().await?;
        recv.recv().await;
        inner_runtime.shutdown_timeout(Duration::from_secs(60));
        Ok(())
    }

    async fn listen_port(&self) -> Result<(), std::io::Error> {
        let listener = TcpListener::bind(self.listen_address).await?;
        let target_address = self.target_address;
        while let Ok((mut ingress, addr)) = listener.accept().await {
            println!("Incoming connection from ingress {}", addr);
            tokio::spawn(async move {
                let mut egress = TcpStream::connect(target_address).await.unwrap();
                match tokio::io::copy_bidirectional(&mut ingress, &mut egress).await {
                    Ok((to_egress, to_ingress)) => {
                        println!(
                            "Connection ended gracefully ({to_egress} bytes from client, {to_ingress} bytes from server)"
                        );
                    }
                    Err(err) => {
                        println!("Error while proxying: {}", err);
                    }
                }
            });
        }
        Ok(())
    }

    fn run(&self) -> Result<(), Box<dyn Error>> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.wait_termination()).map_err(|e| e.into())
    }
}

// Run
pub fn run(args: Args) -> Result<(), Box<dyn Error>> {
    // requested execution state
    let req_es = if args.display {
        println!("Running in ``{}`` mode ==> the machine will not go to sleep and the display will remain on", String::from("Display").green());

        ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED
    } else {
        println!(
            "Running in ``{}`` mode ==> the machine will not go to sleep",
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
    println!(
        "\nSet thread execution state:\n    {} ==> {} ({:#X})\n      {} ==> {} ({:#X})",
        String::from("From").purple(),
        prev_es_label,
        prev_es.0,
        String::from("To").cyan(),
        next_es_label,
        next_es.0
    );

    // After exiting main, StayAwake instance is dropped and the thread execution
    //   state is reset to ES_CONTINUOUS

    WslRunner::new().run()
}
