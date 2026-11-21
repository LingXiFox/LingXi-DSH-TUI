//! Official SDK child-process lifecycle. Wire encoding belongs to dsh-protocol.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use dsh_protocol::RuntimeLock;
use dsh_protocol::protocol::{
    ClientCapabilities, DEFAULT_INITIALIZE_TIMEOUT_SECS, Frame, InitializeParams, InitializeResult,
    JsonRpcId, RUNTIME_SERVER_NAME,
};
use dsh_protocol::stdio::{initialize_request, read_frame, shutdown_request, write_frame};
use dsh_state::TransportState;

const POLL: Duration = Duration::from_millis(25);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Resolved on-disk bundle and route. Launch never installs or fetches anything.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    entry: PathBuf,
    cwd: PathBuf,
    home: PathBuf,
    stderr: PathBuf,
    provider: String,
    model: String,
    initialize_timeout: Duration,
}

impl RuntimeConfig {
    pub fn load(root: &Path) -> Result<Self, String> {
        let (lock_path, runtime_dir) = if root.join("runtime.lock.toml").is_file() {
            (root.join("runtime.lock.toml"), root.join("runtime"))
        } else {
            (
                root.join("runtime/upstream.lock.toml"),
                root.join("runtime/upstream"),
            )
        };
        let lock = RuntimeLock::load(lock_path).map_err(|error| error.to_string())?;
        if lock.product_version() != crate::build_info::VERSION {
            return Err("runtime lock version differs from this TUI build".into());
        }
        let entry = runtime_dir.join(
            lock.packaging
                .runtime_entry
                .as_deref()
                .ok_or("runtime lock has no entry point")?,
        );
        if !entry.is_file() {
            return Err(format!("bundled runtime is not built: {}", entry.display()));
        }
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let state_dir = root.join(".dsh-tui");
        Ok(Self {
            entry: entry.canonicalize().map_err(|error| error.to_string())?,
            cwd,
            home: state_dir.join("agent"),
            stderr: state_dir.join("runtime.log"),
            provider: std::env::var("DSH_TUI_PROVIDER")
                .unwrap_or_else(|_| "deepseek-official".into()),
            model: std::env::var("DSH_TUI_MODEL").unwrap_or_else(|_| "deepseek-v4-pro".into()),
            initialize_timeout: Duration::from_secs(DEFAULT_INITIALIZE_TIMEOUT_SECS),
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new("node");
        command
            .arg(&self.entry)
            .args(["--profile", "sdk-minimal"])
            .current_dir(&self.cwd)
            .env("DSH_HOME", &self.home)
            .env("DSH_TELEMETRY_DISABLED", "1");
        command
    }

    fn params(&self) -> InitializeParams {
        InitializeParams {
            cwd: self.cwd.to_string_lossy().into_owned(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning_effort: None,
            max_tokens: None,
            approval_requests: Some(false),
            capabilities: ClientCapabilities::default(),
        }
    }
}

/// One owned child and its transport-state notifications.
pub struct RuntimeHandle {
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<Result<(), String>>>,
    pub events: tokio::sync::mpsc::UnboundedReceiver<TransportState>,
}

impl RuntimeHandle {
    pub fn start(config: RuntimeConfig) -> io::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        let worker = thread::Builder::new()
            .name("dsh-runtime".into())
            .spawn(move || {
                let _ = events.send(TransportState::Connecting);
                let result = run_child(&config, config.command(), &stopped, &events);
                if let Err(error) = &result {
                    tracing::warn!(%error, "SDK runtime stopped");
                    let _ = events.send(TransportState::Failed(error.clone()));
                }
                result
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
            events: receiver,
        })
    }

    pub async fn shutdown(mut self) -> io::Result<()> {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            tokio::task::spawn_blocking(move || worker.join())
                .await
                .map_err(io::Error::other)?
                .map_err(|_| io::Error::other("runtime worker panicked"))?
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}

impl Drop for RuntimeHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

fn stopped(receiver: &mpsc::Receiver<()>) -> bool {
    !matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty))
}

fn run_child(
    config: &RuntimeConfig,
    mut command: Command,
    stop: &mpsc::Receiver<()>,
    events: &tokio::sync::mpsc::UnboundedSender<TransportState>,
) -> Result<(), String> {
    std::fs::create_dir_all(&config.home).map_err(|error| error.to_string())?;
    let log = File::create(&config.stderr).map_err(|error| error.to_string())?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .map_err(|error| format!("could not start bundled runtime: {error}"))?;
    let result = exchange(&mut child, config, stop, events);
    // Every return path owns cleanup, including spawn setup and handshake failures.
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
    }
    let reaped = child
        .wait()
        .map_err(|error| format!("could not reap runtime: {error}"));
    result.and(reaped.map(|_| ()))
}

fn exchange(
    child: &mut Child,
    config: &RuntimeConfig,
    stop: &mpsc::Receiver<()>,
    events: &tokio::sync::mpsc::UnboundedSender<TransportState>,
) -> Result<(), String> {
    let mut stdin = child.stdin.take().ok_or("runtime stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("runtime stdout unavailable")?;
    let (frames, incoming) = mpsc::sync_channel(64);
    let (requests, outgoing) = mpsc::channel();
    let write_errors = frames.clone();
    // Pipe writes run separately so even a child that never reads stdin can be
    // cancelled and killed by the lifecycle worker within its deadline.
    thread::Builder::new()
        .name("dsh-runtime-stdin".into())
        .spawn(move || {
            for frame in outgoing {
                if let Err(error) = write_frame(&mut stdin, &frame) {
                    let _ = write_errors.try_send(Err(format!("stdin write failed: {error}")));
                    break;
                }
            }
        })
        .map_err(|error| error.to_string())?;
    // A bounded queue protects the UI from a noisy or incompatible child. The
    // reader owns only its pipe and exits when the child closes stdout.
    thread::Builder::new()
        .name("dsh-runtime-stdout".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let frame = read_frame(&mut reader).map_err(|error| error.to_string());
                let done = !matches!(&frame, Ok(Some(_)));
                if frames.send(frame).is_err() || done {
                    break;
                }
            }
        })
        .map_err(|error| error.to_string())?;

    let initialize_id = JsonRpcId::Number(1.into());
    requests
        .send(initialize_request(initialize_id.clone(), &config.params()))
        .map_err(|error| format!("initialize write failed: {error}"))?;
    let mut deadline = Instant::now() + config.initialize_timeout;
    let mut ready = false;
    let mut shutting_down = false;
    let shutdown_id = JsonRpcId::Number(2.into());
    let mut shutdown_acknowledged = false;
    loop {
        if stopped(stop) && !shutting_down {
            if !ready {
                return Ok(());
            }
            requests
                .send(shutdown_request(shutdown_id.clone()))
                .map_err(|error| format!("shutdown write failed: {error}"))?;
            shutting_down = true;
            deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        }
        if (!ready || shutting_down) && Instant::now() >= deadline {
            return Err(if shutting_down {
                "runtime shutdown timed out"
            } else {
                "runtime initialize timed out"
            }
            .into());
        }
        match incoming.recv_timeout(POLL) {
            Ok(Ok(Some(Frame::Response(response)))) => {
                if !ready && response.id == initialize_id {
                    let result: InitializeResult = response.decode_result()?;
                    if result.server_info.name != RUNTIME_SERVER_NAME {
                        return Err(format!(
                            "unexpected SDK server: {}",
                            result.server_info.name
                        ));
                    }
                    ready = true;
                    let _ = events.send(TransportState::Ready {
                        server_name: result.server_info.name,
                        server_version: Some(result.server_info.version),
                    });
                } else if shutting_down && response.id == shutdown_id {
                    if let Some(error) = response.error {
                        return Err(format!("shutdown failed: {}", error.message));
                    }
                    shutdown_acknowledged = true;
                }
                // Drain already buffered frames before observing process exit.
                continue;
            }
            Ok(Ok(Some(Frame::Notification(notification)))) => {
                tracing::debug!(method = %notification.method, "SDK notification");
                continue;
            }
            Ok(Ok(Some(Frame::Request(request)))) => {
                return Err(format!("unsupported SDK host request: {}", request.method));
            }
            Ok(Err(error)) => return Err(format!("runtime transport: {error}")),
            Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if !shutting_down || !shutdown_acknowledged {
                    return Err(
                        "runtime stdout closed before expected response; see .dsh-tui/runtime.log"
                            .into(),
                    );
                }
                // stdout may close just before the child exit becomes observable.
                thread::sleep(POLL);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return if shutting_down && shutdown_acknowledged && status.success() {
                Ok(())
            } else {
                Err(format!(
                    "runtime exited unexpectedly: {status}; see .dsh-tui/runtime.log"
                ))
            };
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
