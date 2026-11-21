//! Binary entry point: terminal setup, event loop, terminal restore.
//!
//! Round one runs the UI against the real runtime lock but no subprocess: the
//! screen reports `detached` until round two wires the SDK transport in.

mod app;
mod build_info;
mod render;
mod terminal;

use std::io::IsTerminal;

use dsh_protocol::lock::{DEFAULT_LOCK_PATH, RuntimeLock};
use dsh_state::AppState;
use tracing_subscriber::EnvFilter;

const DEFAULT_LOG_DIR: &str = ".dsh-tui";

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!(
            "LingXi DSH TUI {} ({})",
            build_info::VERSION,
            build_info::BUILD_ID
        );
        return std::process::ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "LingXi DSH TUI {}\n\nUsage: dsh-tui [--version]\n\n  q / Esc  quit\n  ?        toggle key help\n  f        fold or expand tool rows",
            build_info::VERSION
        );
        return std::process::ExitCode::SUCCESS;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("dsh-tui needs an interactive terminal (stdin and stdout are not a TTY)");
        return std::process::ExitCode::from(2);
    }

    init_logging();

    let state = match load_state() {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(%error, "runtime lock unavailable; starting in degraded mode");
            AppState::unversioned(build_info::BUILD_ID)
        }
    };

    match app::run(state).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dsh-tui failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn load_state() -> Result<AppState, dsh_protocol::LockError> {
    let root = build_info::workspace_root();
    let lock = RuntimeLock::load(root.join(DEFAULT_LOCK_PATH))?;
    Ok(AppState::new(&lock, build_info::BUILD_ID))
}

/// File-based logging keeps stdout clean for the TUI; RUST_LOG opts in.
fn init_logging() {
    let filter = EnvFilter::try_from_env("DSH_TUI_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    let log_dir = build_info::workspace_root().join(DEFAULT_LOG_DIR);
    if std::fs::create_dir_all(&log_dir).is_ok() {
        let file = std::fs::File::create(log_dir.join("dsh-tui.log"));
        if let Ok(file) = file {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
            return;
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::sink)
        .init();
}
