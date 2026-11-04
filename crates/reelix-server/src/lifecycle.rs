//! Process-wide shutdown and restart requests.
//!
//! [`crate::server::run`] serves until [`shutdown_token`] is cancelled (Ctrl-C, SIGTERM, a Windows
//! service Stop, or [`request_restart`]). After `run` has finished its graceful shutdown, `main`
//! (or the Windows service entry point) checks [`take_restart`] and starts the new binary.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tokio_util::sync::CancellationToken;

/// Set once a self-update has swapped the files and asked for a restart.
pub static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

static SHUTDOWN: OnceLock<CancellationToken> = OnceLock::new();
static RESTART_EXE: Mutex<Option<PathBuf>> = Mutex::new(None);
static SERVICE_MODE: AtomicBool = AtomicBool::new(false);

/// The token that stops the server; cancelling it starts the graceful shutdown.
pub fn shutdown_token() -> CancellationToken {
    SHUTDOWN.get_or_init(CancellationToken::new).clone()
}

/// Stops the server and, once its shutdown is complete, starts `exe` in its place.
pub fn request_restart(exe: PathBuf) {
    *RESTART_EXE.lock().unwrap_or_else(|e| e.into_inner()) = Some(exe);
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
    shutdown_token().cancel();
}

/// The binary to restart into, if a restart was requested.
pub fn take_restart() -> Option<PathBuf> {
    if !RESTART_REQUESTED.load(Ordering::SeqCst) {
        return None;
    }
    RESTART_EXE.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Marks the process as running under the Windows service manager (`reelix service run`).
pub fn set_service_mode() {
    SERVICE_MODE.store(true, Ordering::SeqCst);
}

/// True under the Windows service manager, which restarts the service itself (see
/// `crate::service`).
pub fn service_mode() -> bool {
    SERVICE_MODE.load(Ordering::SeqCst)
}

/// Replaces this process with `exe`, passing the same arguments. Logging has shut down by now,
/// so problems go to stderr.
///
/// On Unix the new binary is `exec`ed: same PID, so systemd (or the start script's shell) sees
/// no exit. Windows has no `exec`: the new process is spawned with the same arguments, working
/// directory and console, and this one exits.
pub fn restart(exe: &std::path::Path) -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    eprintln!("reelix: restarting into {}", exe.display());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only returns on failure.
        let err = std::process::Command::new(exe).args(&args).exec();
        eprintln!("reelix: could not start the updated binary: {err}");
        ExitCode::FAILURE
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(exe).args(&args).spawn() {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("reelix: could not start the updated binary: {e}");
                ExitCode::FAILURE
            }
        }
    }
}
