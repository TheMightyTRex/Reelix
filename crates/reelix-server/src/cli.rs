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
    /// Authenticate Spotify with native OAuth/PKCE and save reusable session credentials.
    SpotifyLogin {
        #[arg(long)]
        credentials: PathBuf,
        /// Optional Spotify developer application client ID.
        #[arg(long)]
        client_id: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:8898/login")]
        redirect_uri: String,
    },
    /// Ask the install wizard's questions in this terminal, for a server with no desktop. Needs
    /// Reelix running (with Docker: `docker exec -it reelix /app/reelix setup`).
    Setup {
        /// Plain lines that scroll, with nothing redrawn: for screen readers (or REELIX_PLAIN=1).
        #[arg(long)]
        plain: bool,
        /// Reelix's address (default: this server, on --port).
        #[arg(long, value_name = "URL")]
        url: Option<String>,
    },
    /// Manage the Reelix Windows service (Windows only).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum ServiceAction {
    /// Register and start the "Reelix" service (`--data-dir`, default C:\ProgramData\Reelix, and
    /// `--port` are passed on to it). Needs an elevated prompt. Idempotent: an existing service is
    /// reconfigured (and started if stopped). Exits non-zero, with the error on stderr, on failure.
    Install,
    /// Stop and remove the service; the data directory is kept.
    Uninstall,
    /// Run as the service (started by the Windows service manager).
    Run,
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
        let cli = Cli::parse_from(["reelix", "spotify-login", "--credentials", "credentials.json"]);
        assert!(matches!(cli.command, Some(Command::SpotifyLogin { .. })));
        assert!(Cli::try_parse_from(["reelix", "spotify-login"]).is_err());
        let cli = Cli::parse_from([
            "reelix",
            "service",
            "install",
            "--data-dir",
            r"C:\ProgramData\Reelix",
            "--port",
            "7701",
        ]);
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Install
            })
        ));
        assert_eq!(cli.port, Some(7701));
        assert!(cli.data_dir.is_some());
        let cli = Cli::parse_from(["reelix", "service", "run", "--data-dir", "x"]);
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Run
            })
        ));
        assert!(Cli::try_parse_from(["reelix", "service"]).is_err());
        assert!(Cli::try_parse_from(["reelix", "--port", "notaport"]).is_err());
    }
}
