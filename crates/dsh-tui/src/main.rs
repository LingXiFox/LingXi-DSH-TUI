//! Minimal entry point after the deliberate UI reset.
//! The official Agent integration remains in runtime.rs; a new UI will be built separately.

mod build_info;
mod runtime;
mod terminal;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use dsh_state::TransportState;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tracing_subscriber::EnvFilter;

const DEFAULT_LOG_DIR: &str = ".dsh-tui";

#[tokio::main]
async fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if matches!(args.as_slice(), [arg] if arg == "--version" || arg == "-V") {
        println!("LingXi DSH TUI {} ({})", build_info::VERSION, build_info::BUILD_ID);
        return ExitCode::SUCCESS;
    }
    if matches!(args.as_slice(), [arg] if arg == "--help" || arg == "-h") {
        println!("LingXi DSH TUI {}\n\nUsage: dsh-tui [--version | --check-runtime]\n\nThe legacy UI has been removed for a clean-room redesign.\nThe empty-screen scaffold exits with q, Esc, or Ctrl+C.", build_info::VERSION);
        return ExitCode::SUCCESS;
    }
    if matches!(args.as_slice(), [arg] if arg == "--check-runtime") {
        init_logging();
        return match check_runtime().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("runtime check failed: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if !args.is_empty() {
        eprintln!("unsupported option during UI reset; see dsh-tui --help");
        return ExitCode::from(2);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("dsh-tui requires an interactive TTY");
        return ExitCode::from(2);
    }

    init_logging();
    match placeholder_terminal() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("terminal failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn draw_placeholder(frame: &mut Frame<'_>) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("LingXi DSH-TUI"),
            Line::from(""),
            Line::from("UI clean slate — renderer intentionally removed."),
            Line::from("Official SDK runtime, protocol and session journal are preserved."),
            Line::from(""),
            Line::from("q / Esc / Ctrl+C  Exit"),
        ]),
        frame.area(),
    );
}

fn placeholder_terminal() -> io::Result<()> {
    let mut terminal = terminal::TerminalSession::enter()?;
    let result = (|| -> io::Result<()> {
        terminal.draw(draw_placeholder)?;
        loop {
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && (matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL))) =>
                {
                    break;
                }
                Event::Resize(_, _) => terminal.draw(draw_placeholder)?,
                _ => {}
            }
        }
        Ok(())
    })();
    let restoration = terminal.restore();
    result.and(restoration)
}

async fn check_runtime() -> Result<(), String> {
    let config = runtime::RuntimeConfig::load(&build_info::workspace_root())?;
    let mut sdk = runtime::RuntimeHandle::start(config).map_err(|error| error.to_string())?;
    let result = loop {
        match sdk.events.recv().await {
            Some(TransportState::Ready { server_name, server_version }) => {
                println!("SDK initialized: {server_name} {}", server_version.unwrap_or_default());
                break Ok(());
            }
            Some(TransportState::Failed(error)) => break Err(error),
            Some(_) => {}
            None => break Err("runtime worker closed without a handshake result".into()),
        }
    };
    let shutdown = sdk.shutdown(|_| {}).await.map_err(|error| error.to_string());
    result.and(shutdown)?;
    println!("SDK shutdown: ok");
    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_env("DSH_TUI_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    let log_dir = build_info::workspace_root().join(DEFAULT_LOG_DIR);
    if std::fs::create_dir_all(&log_dir).is_ok() {
        if let Ok(file) = std::fs::File::create(log_dir.join("dsh-tui.log")) {
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
        .with_writer(io::sink)
        .init();
}
