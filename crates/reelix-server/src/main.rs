use std::process::ExitCode;

use clap::Parser;
use reelix_server::cli::{Cli, Command};
use reelix_server::config::Config;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match Config::load(&cli.overrides()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("reelix: invalid configuration: {e}");
            return ExitCode::from(2);
        }
    };

    match cli.command {
        Some(Command::Healthcheck) => ExitCode::from(reelix_server::healthcheck::run(&config)),
        None => {
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("reelix: failed to start async runtime: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match runtime.block_on(reelix_server::server::run(config)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    // Logging may not be initialised yet, so always print to stderr too.
                    tracing::error!("fatal: {e:#}");
                    eprintln!("reelix: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
