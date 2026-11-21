//! Binary entry point: terminal setup, event loop, terminal restore.
//!
//! The bundled official Agent runs behind its SDK stdio process boundary.

mod app;
mod build_info;
mod render;
mod runtime;
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
            "LingXi DSH TUI {}\n\nUsage: dsh-tui [--version | --check-runtime]\n\n  --check-runtime  verify bundled SDK initialization and shutdown\n  Ctrl+P           command palette\n  Ctrl+J           insert newline\n  /quit            quit",
            build_info::VERSION
        );
        return std::process::ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "--check-runtime") {
        init_logging();
        return match check_runtime().await {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("runtime check failed: {error}");
                std::process::ExitCode::FAILURE
            }
        };
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

    let config = runtime::RuntimeConfig::load(&build_info::workspace_root());
    match app::run(state, config).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dsh-tui failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn load_state() -> Result<AppState, dsh_protocol::LockError> {
    let root = build_info::workspace_root();
    let path = if root.join("runtime.lock.toml").is_file() {
        root.join("runtime.lock.toml")
    } else {
        root.join(DEFAULT_LOCK_PATH)
    };
    let lock = RuntimeLock::load(path)?;
    Ok(AppState::new(&lock, build_info::BUILD_ID))
}

async fn check_runtime() -> Result<(), String> {
    let config = runtime::RuntimeConfig::load(&build_info::workspace_root())?;
    let mut runtime = runtime::RuntimeHandle::start(config).map_err(|error| error.to_string())?;
    let result = loop {
        match runtime.events.recv().await {
            Some(dsh_state::TransportState::Ready {
                server_name,
                server_version,
            }) => {
                println!(
                    "SDK initialized: {server_name} {}",
                    server_version.unwrap_or_default()
                );
                break Ok(());
            }
            Some(dsh_state::TransportState::Failed(error)) => break Err(error),
            Some(_) => {}
            None => break Err("runtime worker closed without a handshake result".into()),
        }
    };
    let shutdown = runtime
        .shutdown(|_| {})
        .await
        .map_err(|error| error.to_string());
    result.and(shutdown)?;
    println!("SDK shutdown: ok");
    Ok(())
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
