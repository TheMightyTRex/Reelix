//! Command-line interface.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::CliOverrides;

/// Reelix: movies, series, indexers and requests in one server.
#[derive(Debug, Parser)]
#[command(name = "reelix", version = reelix_core::VERSION, about)]
pub struct Cli {
    /// Path to config.toml (default: <data-dir>/config.toml).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Data directory for the database, keys and logs (default: ./data).
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// HTTP port (default: 7700).
    #[arg(long, global = true)]
    pub port: Option<u16>,
    /// Address to bind (default: 0.0.0.0).
    #[arg(long, global = true, value_name = "IP")]
    pub bind: Option<IpAddr>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Probe the local health endpoint; exits 0 iff it returns HTTP 200.
    Healthcheck,
}

impl Cli {
    pub fn overrides(&self) -> CliOverrides {
        CliOverrides {
            config: self.config.clone(),
            data_dir: self.data_dir.clone(),
            port: self.port,
            bind: self.bind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_valid_and_parses() {
        Cli::command().debug_assert();
        let cli = Cli::parse_from([
            "reelix",
            "--port",
            "8000",
            "--bind",
            "127.0.0.1",
            "--data-dir",
            "/x",
        ]);
        assert_eq!(cli.port, Some(8000));
        assert!(cli.command.is_none());
        let cli = Cli::parse_from(["reelix", "healthcheck", "--port", "9"]);
        assert!(matches!(cli.command, Some(Command::Healthcheck)));
        assert_eq!(cli.overrides().port, Some(9));
        assert!(Cli::try_parse_from(["reelix", "--port", "notaport"]).is_err());
    }
}
