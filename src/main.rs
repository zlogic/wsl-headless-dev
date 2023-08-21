use std::process;
use wsl_headless_dev::Args;

fn main() {
    let args = Args::parse();
    if let Err(e) = wsl_headless_dev::run(args) {
        println!("Stopping with error: {}", e);
        process::exit(1);
    }
    process::exit(0);
}
