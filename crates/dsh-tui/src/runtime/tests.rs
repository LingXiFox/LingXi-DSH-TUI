//! Process fixtures exercise the actual pipe and cleanup paths without credentials.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const READY: &str = r#"read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"deepseek-harness-sdk-runtime","version":"0.0.1"}}}'
"#;
const SHUTDOWN: &str = r#"read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
"#;

fn fixture() -> RuntimeConfig {
    let home = std::env::temp_dir().join(format!(
        "dsh-rpc-test-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    RuntimeConfig {
        entry: home.join("unused.js"),
        cwd: std::env::current_dir().unwrap(),
        stderr: home.join("runtime.log"),
        home,
        provider: "deepseek-official".into(),
        model: "deepseek-v4-pro".into(),
        initialize_timeout: Duration::from_millis(300),
    }
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    command
}

#[test]
fn initializes_and_shuts_down_over_real_pipes() {
    let config = fixture();
    let (stop, stopped) = mpsc::channel();
    let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let script = format!("{READY}{SHUTDOWN}");
    let worker_config = config.clone();
    let worker =
        thread::spawn(move || run_child(&worker_config, shell(&script), &stopped, &events));
    let state = receiver.blocking_recv().unwrap();
    assert_eq!(
        state,
        TransportState::Ready {
            server_name: RUNTIME_SERVER_NAME.into(),
            server_version: Some("0.0.1".into())
        }
    );
    stop.send(()).unwrap();
    worker.join().unwrap().unwrap();
    std::fs::remove_dir_all(config.home).unwrap();
}

#[test]
fn rejects_rpc_errors_noise_wrong_identity_truncation_and_early_exit() {
    for (script, expected) in [
        (
            r#"read -r request; printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"route unavailable"}}'"#,
            "route unavailable",
        ),
        (
            r#"read -r request; printf '%s\n' 'invalid stdout'"#,
            "runtime transport:",
        ),
        (
            r#"read -r request; printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"other","version":"1"}}}'"#,
            "unexpected SDK server",
        ),
        (
            r#"read -r request; printf '%s' '{"jsonrpc":'"#,
            "before newline",
        ),
        ("read -r request; exit 7", "closed before expected response"),
        (
            r#"read -r request; printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{}}'"#,
            "invalid SDK result",
        ),
        (
            r#"read -r request; printf '%s\n' '{"jsonrpc":"2.0","id":5,"result":{}}'; read -r next"#,
            "initialize timed out",
        ),
        ("read -r request; read -r next", "initialize timed out"),
    ] {
        let config = fixture();
        let (_stop, stopped) = mpsc::channel();
        let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let error = run_child(&config, shell(script), &stopped, &events).unwrap_err();
        assert!(
            error.contains(expected),
            "expected {expected:?}, got {error:?}"
        );
        std::fs::remove_dir_all(config.home).unwrap();
    }
}

#[test]
fn cancellation_interrupts_a_pending_initialize() {
    let config = fixture();
    let (stop, stopped) = mpsc::channel();
    let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
    stop.send(()).unwrap();
    run_child(
        &config,
        shell("read -r request; read -r next"),
        &stopped,
        &events,
    )
    .unwrap();
    std::fs::remove_dir_all(config.home).unwrap();
}

#[test]
fn initialize_timeout_interrupts_a_blocked_stdin_write() {
    let mut config = fixture();
    config.model = "x".repeat(2 * 1024 * 1024);
    let (_stop, stopped) = mpsc::channel();
    let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let start = Instant::now();
    let error = run_child(&config, shell("kill -STOP $$"), &stopped, &events).unwrap_err();
    assert!(error.contains("initialize timed out"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2));
    std::fs::remove_dir_all(config.home).unwrap();
}

#[test]
fn shutdown_timeout_kills_and_reaps_the_child() {
    let config = fixture();
    let (stop, stopped) = mpsc::channel();
    let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let worker_config = config.clone();
    let worker = thread::spawn(move || {
        run_child(
            &worker_config,
            shell(&format!("{READY}read -r shutdown; read -r never")),
            &stopped,
            &events,
        )
    });
    assert!(matches!(
        receiver.blocking_recv(),
        Some(TransportState::Ready { .. })
    ));
    stop.send(()).unwrap();
    let start = Instant::now();
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .contains("shutdown timed out")
    );
    assert!(start.elapsed() < SHUTDOWN_TIMEOUT + Duration::from_secs(2));
    std::fs::remove_dir_all(config.home).unwrap();
}

#[test]
fn locates_workspace_and_packaged_runtime_without_spawning() {
    let config = fixture();
    std::fs::create_dir_all(&config.home).unwrap();
    let root = &config.home;
    let lock = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime/upstream.lock.toml"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("runtime/upstream/apps/cli/lib")).unwrap();
    std::fs::write(root.join("runtime/upstream.lock.toml"), &lock).unwrap();
    let workspace_entry = root.join("runtime/upstream/apps/cli/lib/bin.js");
    assert!(
        RuntimeConfig::load(root)
            .unwrap_err()
            .contains("is not built")
    );
    std::fs::write(&workspace_entry, "").unwrap();
    assert_eq!(
        RuntimeConfig::load(root).unwrap().entry,
        workspace_entry.canonicalize().unwrap()
    );
    std::fs::create_dir_all(root.join("runtime/apps/cli/lib")).unwrap();
    std::fs::write(root.join("runtime.lock.toml"), &lock).unwrap();
    let bundle_entry = root.join("runtime/apps/cli/lib/bin.js");
    std::fs::write(&bundle_entry, "").unwrap();
    assert_eq!(
        RuntimeConfig::load(root).unwrap().entry,
        bundle_entry.canonicalize().unwrap()
    );
    std::fs::remove_dir_all(config.home).unwrap();
}
