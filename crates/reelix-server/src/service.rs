//! `reelix service install|uninstall|run`: running Reelix as the Windows service "Reelix".
//!
//! `install` registers the service (auto start, LocalSystem) with the command line
//! `"<exe>" service run --data-dir "<dir>" [--port N]` and failure actions that restart it after
//! 5 s. `run` is what the service manager starts: it serves like a normal start and maps the
//! Stop and Shutdown controls to the graceful shutdown.
//!
//! Self-update restarts: Windows services can't `exec`, so after an update has swapped the files
//! the service reports Stopped with a non-zero exit code. Because `install` also enables failure
//! actions for such "non-crash failures", the service manager then starts the service again from
//! the same path, which now holds the new binary.

use std::process::ExitCode;

use crate::config::Config;

/// Name and display name of the service.
pub const SERVICE_NAME: &str = "Reelix";

/// `reelix service ...` on this OS.
#[cfg(not(windows))]
pub fn main(_action: &crate::cli::ServiceAction, _config: Config, _cli: &crate::cli::Cli) -> ExitCode {
    eprintln!("reelix: `reelix service` is only available on Windows");
    ExitCode::from(2)
}

/// `reelix service ...` on this OS.
#[cfg(windows)]
pub fn main(action: &crate::cli::ServiceAction, config: Config, cli: &crate::cli::Cli) -> ExitCode {
    use crate::cli::ServiceAction;
    let result = match action {
        ServiceAction::Install => windows::install(cli.data_dir.as_deref(), cli.port),
        ServiceAction::Uninstall => windows::uninstall(),
        ServiceAction::Run => return windows::run(config),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reelix: {e:#}");
            if e.chain().any(|c| {
                c.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
            }) {
                eprintln!("reelix: run this from an elevated (administrator) prompt");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use anyhow::Context;
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
        ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo,
        ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use super::SERVICE_NAME;
    use crate::config::Config;

    /// Service-specific exit code asking the service manager to start the (updated) binary again.
    const EXIT_RESTART: u32 = 1;
    /// Service-specific exit code for a server that stopped with an error.
    const EXIT_FAILED: u32 = 2;
    const RESTART_DELAY: Duration = Duration::from_millis(5000);

    fn default_data_dir() -> PathBuf {
        std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("Reelix")
    }

    pub fn install(data_dir: Option<&Path>, port: Option<u16>) -> anyhow::Result<()> {
        let exe = std::env::current_exe().context("locating reelix.exe")?;
        let data_dir = match data_dir {
            Some(d) => std::path::absolute(d).context("resolving the data directory")?,
            None => default_data_dir(),
        };
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("creating the data directory {}", data_dir.display()))?;
        let mut launch_arguments: Vec<OsString> = vec![
            "service".into(),
            "run".into(),
            "--data-dir".into(),
            data_dir.clone().into(),
        ];
        if let Some(port) = port {
            launch_arguments.extend(["--port".into(), port.to_string().into()]);
        }
        let info = ServiceInfo {
            name: SERVICE_NAME.into(),
            display_name: SERVICE_NAME.into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments,
            dependencies: vec![],
            // LocalSystem.
            account_name: None,
            account_password: None,
        };
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )
        .map_err(io_error)
        .context("connecting to the service manager")?;
        let access = ServiceAccess::QUERY_STATUS
            | ServiceAccess::START
            | ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::QUERY_CONFIG;
        // Installing again (e.g. from the installer's upgrade) updates the existing service.
        let service = match manager.open_service(SERVICE_NAME, access) {
            Ok(service) => {
                service
                    .change_config(&info)
                    .map_err(io_error)
                    .context("updating the service")?;
                service
            }
            Err(_) => manager
                .create_service(&info, access)
                .map_err(io_error)
                .context("creating the service")?,
        };
        let _ = service.set_description("Reelix media manager");
        let restart = ServiceAction {
            action_type: ServiceActionType::Restart,
            delay: RESTART_DELAY,
        };
        service
            .update_failure_actions(ServiceFailureActions {
                reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
                reboot_msg: None,
                command: None,
                actions: Some(vec![restart.clone(), restart.clone(), restart]),
            })
            .map_err(io_error)
            .context("setting the restart-on-failure actions")?;
        // Also run the failure actions when the service stops itself with an error exit code,
        // which is how a self-update restarts it (see the module docs).
        service
            .set_failure_actions_on_non_crash_failures(true)
            .map_err(io_error)
            .context("setting the restart-on-failure actions")?;
        let running = service
            .query_status()
            .is_ok_and(|s| s.current_state != ServiceState::Stopped);
        if !running {
            service
                .start::<&str>(&[])
                .map_err(io_error)
                .context("starting the service")?;
        }
        println!(
            "Installed and started the {SERVICE_NAME} service (data directory {}).",
            data_dir.display()
        );
        Ok(())
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(io_error)
            .context("connecting to the service manager")?;
        let Ok(service) = manager.open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        ) else {
            println!("The {SERVICE_NAME} service is not installed.");
            return Ok(());
        };
        let status = service.query_status().map_err(io_error)?;
        if status.current_state != ServiceState::Stopped {
            let _ = service.stop();
            let deadline = Instant::now() + Duration::from_secs(60);
            while service
                .query_status()
                .is_ok_and(|s| s.current_state != ServiceState::Stopped)
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        service
            .delete()
            .map_err(io_error)
            .context("deleting the service")?;
        println!("Removed the {SERVICE_NAME} service; the data directory was kept.");
        Ok(())
    }

    /// Turns `windows_service` errors into I/O errors, so "access denied" can be recognised.
    fn io_error(e: windows_service::Error) -> anyhow::Error {
        match e {
            windows_service::Error::Winapi(io) => io.into(),
            other => anyhow::Error::new(other),
        }
    }

    static CONFIG: OnceLock<Config> = OnceLock::new();

    /// Entry point used by the service manager; blocks until the service has stopped.
    pub fn run(config: Config) -> ExitCode {
        let _ = CONFIG.set(config);
        match dispatch::start() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("reelix: `reelix service run` is started by the Windows service manager: {e}");
                ExitCode::FAILURE
            }
        }
    }

    /// The `windows-service` macro generates the `extern "system"` entry point the service
    /// manager calls. Its `unsafe` block (parsing the raw argument array) comes from the external
    /// macro, which the workspace's `unsafe_code = "forbid"` doesn't flag, so no lint exception
    /// is needed. No unsafe code is written here.
    mod dispatch {
        windows_service::define_windows_service!(ffi_service_main, service_main);

        /// Hands this thread to the service dispatcher until the service stops.
        pub(super) fn start() -> windows_service::Result<()> {
            windows_service::service_dispatcher::start(super::SERVICE_NAME, ffi_service_main)
        }

        fn service_main(_arguments: Vec<std::ffi::OsString>) {
            super::service_main();
        }
    }

    fn status(state: ServiceState, exit_code: ServiceExitCode, wait_hint: Duration) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code,
            checkpoint: 0,
            wait_hint,
            process_id: None,
        }
    }

    fn service_main() {
        crate::lifecycle::set_service_mode();
        let shutdown = crate::lifecycle::shutdown_token();
        let on_control = {
            let shutdown = shutdown.clone();
            move |control| match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    shutdown.cancel();
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let Ok(handle) = service_control_handler::register(SERVICE_NAME, on_control) else {
            return;
        };
        let report = |s| {
            let _ = handle.set_service_status(s);
        };
        let Some(config) = CONFIG.get().cloned() else {
            report(status(
                ServiceState::Stopped,
                ServiceExitCode::ServiceSpecific(EXIT_FAILED),
                Duration::ZERO,
            ));
            return;
        };
        // The service starts in System32; relative paths (e.g. `web/`) mean the install folder.
        if let Some(dir) = crate::maintenance::self_update::install_dir() {
            let _ = std::env::set_current_dir(dir);
        }
        report(status(
            ServiceState::Running,
            ServiceExitCode::NO_ERROR,
            Duration::ZERO,
        ));

        let exit_code = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(runtime) => {
                let stopping = shutdown.clone();
                runtime.spawn(async move {
                    stopping.cancelled().await;
                    let _ = handle.set_service_status(status(
                        ServiceState::StopPending,
                        ServiceExitCode::NO_ERROR,
                        Duration::from_secs(30),
                    ));
                });
                let result = runtime.block_on(crate::server::run(config));
                runtime.shutdown_timeout(Duration::from_secs(5));
                match result {
                    // Stopping with an error code makes the service manager start the new binary.
                    Ok(()) if crate::lifecycle::take_restart().is_some() => {
                        ServiceExitCode::ServiceSpecific(EXIT_RESTART)
                    }
                    Ok(()) => ServiceExitCode::NO_ERROR,
                    Err(e) => {
                        tracing::error!("fatal: {e:#}");
                        ServiceExitCode::ServiceSpecific(EXIT_FAILED)
                    }
                }
            }
            Err(_) => ServiceExitCode::ServiceSpecific(EXIT_FAILED),
        };
        report(status(ServiceState::Stopped, exit_code, Duration::ZERO));
    }
}
