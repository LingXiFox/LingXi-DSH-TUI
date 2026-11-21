//! Official SDK child-process lifecycle. Wire encoding belongs to dsh-protocol.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use dsh_protocol::RuntimeLock;
use dsh_protocol::protocol::{
    ClientCapabilities, ClientMethod, DEFAULT_INITIALIZE_TIMEOUT_SECS, Frame, InitializeParams,
    InitializeResult, JsonRpcId, METHOD_APPROVAL_REQUEST, METHOD_USER_QUESTION,
    RUNTIME_SERVER_NAME, Response,
};
use dsh_protocol::stdio::{initialize_request, read_frame, request, shutdown_request, write_frame};
use dsh_state::TransportState;
use serde_json::{Value, json};
use tokio::sync::{mpsc as async_mpsc, oneshot};

const POLL: Duration = Duration::from_millis(25);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const QUEUE: usize = 32;
const RPC_LIMIT: usize = 32;
const INTERACTION_TIMEOUT: Duration = Duration::from_secs(120);

pub type RpcResult = oneshot::Receiver<Result<Response, String>>;

enum Outbound {
    Call {
        method: ClientMethod,
        params: Value,
        deadline: Instant,
        reply: oneshot::Sender<Result<Response, String>>,
    },
    Answer(JsonRpcId, Value),
}

struct WorkerQueues {
    stop: mpsc::Receiver<()>,
    outbound: mpsc::Receiver<Outbound>,
    events: async_mpsc::UnboundedSender<TransportState>,
    inbox: async_mpsc::Sender<Inbound>,
}

pub struct Inbound {
    pub frame: Frame,
    pub interaction_deadline: Option<Instant>,
}

/// Resolved on-disk bundle and route. Launch never installs or fetches anything.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    entry: PathBuf,
    pub cwd: PathBuf,
    home: PathBuf,
    stderr: PathBuf,
    pub provider: String,
    pub model: String,
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
            .args(["--profile", "sdk"])
            .current_dir(&self.cwd)
            .env("DSH_HOME", &self.home)
            .env("DSH_TELEMETRY_DISABLED", "1")
            .env("DSH_PERMISSION_MODE", "workspace-write");
        command
    }

    fn params(&self) -> InitializeParams {
        InitializeParams {
            cwd: self.cwd.to_string_lossy().into_owned(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning_effort: None,
            max_tokens: None,
            approval_requests: Some(true),
            capabilities: ClientCapabilities {
                user_questions: true,
            },
        }
    }
}

/// One owned child and its transport-state notifications.
pub struct RuntimeHandle {
    stop: mpsc::Sender<()>,
    outbound: mpsc::SyncSender<Outbound>,
    worker: Option<thread::JoinHandle<Result<(), String>>>,
    pub events: tokio::sync::mpsc::UnboundedReceiver<TransportState>,
    pub inbox: async_mpsc::Receiver<Inbound>,
}

impl RuntimeHandle {
    pub fn start(config: RuntimeConfig) -> io::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (outbound, outgoing) = mpsc::sync_channel(QUEUE);
        let (inbox, incoming) = async_mpsc::channel(QUEUE);
        let worker = thread::Builder::new()
            .name("dsh-runtime".into())
            .spawn(move || {
                let queues = WorkerQueues {
                    stop: stopped,
                    outbound: outgoing,
                    events,
                    inbox,
                };
                let _ = queues.events.send(TransportState::Connecting);
                let result = run_child(&config, config.command(), &queues);
                if let Err(error) = &result {
                    tracing::warn!(%error, "SDK runtime stopped");
                    let _ = queues.events.send(TransportState::Failed(error.clone()));
                }
                result
            })?;
        Ok(Self {
            stop,
            outbound,
            worker: Some(worker),
            events: receiver,
            inbox: incoming,
        })
    }

    pub fn call(
        &self,
        method: ClientMethod,
        params: Value,
        timeout: Duration,
    ) -> Result<RpcResult, String> {
        let (reply, result) = oneshot::channel();
        self.enqueue(Outbound::Call {
            method,
            params,
            deadline: Instant::now() + timeout,
            reply,
        })?;
        Ok(result)
    }

    pub fn answer(&self, id: JsonRpcId, result: Value) -> Result<(), String> {
        self.enqueue(Outbound::Answer(id, result))
    }

    fn enqueue(&self, outbound: Outbound) -> Result<(), String> {
        self.outbound
            .try_send(outbound)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "SDK request queue is full".into(),
                mpsc::TrySendError::Disconnected(_) => "SDK transport is closed".into(),
            })
    }

    pub async fn shutdown(mut self, mut receive: impl FnMut(Inbound)) -> io::Result<()> {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            while let Some(frame) = self.inbox.recv().await {
                receive(frame);
            }
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
    queues: &WorkerQueues,
) -> Result<(), String> {
    std::fs::create_dir_all(&config.home).map_err(|error| error.to_string())?;
    let log = File::create(&config.stderr).map_err(|error| error.to_string())?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .map_err(|error| format!("could not start bundled runtime: {error}"))?;
    let result = exchange(&mut child, config, queues);
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
    queues: &WorkerQueues,
) -> Result<(), String> {
    let mut stdin = child.stdin.take().ok_or("runtime stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("runtime stdout unavailable")?;
    let (frames, incoming) = mpsc::sync_channel(QUEUE);
    let (requests, outgoing) = mpsc::sync_channel(QUEUE);
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

    let initialize_id = JsonRpcId::from(1);
    send(
        &requests,
        initialize_request(initialize_id.clone(), &config.params()),
    )?;
    let mut deadline = Instant::now() + config.initialize_timeout;
    let mut ready = false;
    let mut shutting_down = false;
    let shutdown_id = JsonRpcId::from(2);
    let mut shutdown_acknowledged = false;
    let mut next_id = 3;
    let mut calls: HashMap<u64, PendingCall> = HashMap::new();
    let mut interactions: Vec<PendingInteraction> = Vec::new();
    let mut deliver = None;
    loop {
        if stopped(&queues.stop) && !shutting_down {
            if !ready {
                return Ok(());
            }
            send(&requests, shutdown_request(shutdown_id.clone()))?;
            shutting_down = true;
            deadline = Instant::now() + SHUTDOWN_TIMEOUT;
            for (_, call) in calls.drain() {
                let _ = call.reply.send(Err("SDK is shutting down".into()));
            }
        }
        if (!ready || shutting_down) && Instant::now() >= deadline {
            return Err(if shutting_down {
                "runtime shutdown timed out"
            } else {
                "runtime initialize timed out"
            }
            .into());
        }
        if ready && !shutting_down {
            let now = Instant::now();
            calls.retain(|_, call| !call.reply.is_closed());
            let expired = calls.iter().find(|(_, call)| now >= call.deadline);
            if let Some((&id, call)) = expired {
                let error = format!(
                    "SDK {} timed out; its outcome is unknown",
                    call.method.as_str()
                );
                if let Some(call) = calls.remove(&id) {
                    let _ = call.reply.send(Err(error));
                }
            }
            if let Some(index) = interactions.iter().position(|item| now >= item.deadline) {
                let item = interactions.remove(index);
                send(
                    &requests,
                    Response::success(
                        item.id,
                        if item.question.is_none() {
                            json!({"outcome":"cancelled"})
                        } else {
                            json!({"cancelled":true})
                        },
                    ),
                )?;
            }
            if let Ok(outbound) = queues.outbound.try_recv() {
                match outbound {
                    Outbound::Call {
                        method,
                        params,
                        deadline,
                        reply,
                    } => {
                        if reply.is_closed() {
                            continue;
                        }
                        if calls.len() >= RPC_LIMIT || now >= deadline {
                            let _ = reply.send(Err(
                                "SDK request expired or capacity reached before sending".into(),
                            ));
                        } else {
                            let id = next_id;
                            next_id += 1;
                            send(&requests, request(id.into(), method, Some(params)))?;
                            calls.insert(
                                id,
                                PendingCall {
                                    method,
                                    deadline,
                                    reply,
                                },
                            );
                        }
                    }
                    Outbound::Answer(id, result) => {
                        if let Some(index) = interactions.iter().position(|item| item.id == id) {
                            interactions.remove(index);
                            send(&requests, Response::success(id, result))?;
                        }
                    }
                }
            }
        }
        if let Some(frame) = deliver.take() {
            match queues.inbox.try_send(frame) {
                Ok(()) => {}
                Err(async_mpsc::error::TrySendError::Full(frame)) => {
                    deliver = Some(frame);
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(async_mpsc::error::TrySendError::Closed(_)) => {}
            }
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
                    let _ = queues.events.send(TransportState::Ready {
                        server_name: result.server_info.name,
                        server_version: Some(result.server_info.version),
                    });
                } else if shutting_down && response.id == shutdown_id {
                    response.decode_result::<Value>()?;
                    shutdown_acknowledged = true;
                } else if let Some(call) = response.id.as_u64().and_then(|id| calls.remove(&id)) {
                    let _ = call.reply.send(Ok(response));
                }
                // Drain already buffered frames before observing process exit.
                continue;
            }
            Ok(Ok(Some(Frame::Notification(notification)))) => {
                if notification.method
                    == dsh_protocol::protocol::ServerNotification::UserQuestionCancelled.as_str()
                {
                    if let Some(id) = notification
                        .params
                        .as_ref()
                        .and_then(|p| p["requestId"].as_str())
                    {
                        interactions.retain(|item| item.question.as_deref() != Some(id));
                    }
                }
                deliver = Some(Inbound {
                    frame: Frame::Notification(notification),
                    interaction_deadline: None,
                });
                continue;
            }
            Ok(Ok(Some(Frame::Request(request)))) => {
                if shutting_down {
                    deliver = Some(Inbound {
                        frame: Frame::Request(request),
                        interaction_deadline: None,
                    });
                    continue;
                }
                let approval = request.method == METHOD_APPROVAL_REQUEST;
                let question = request
                    .params
                    .as_ref()
                    .and_then(|p| p["requestId"].as_str());
                let mut interaction_deadline = None;
                if !ready || (!approval && request.method != METHOD_USER_QUESTION) {
                    send(
                        &requests,
                        Response::failure(
                            request.id.clone(),
                            -32601,
                            "unsupported SDK host request",
                        ),
                    )?;
                } else if !approval && question.is_none() {
                    send(
                        &requests,
                        Response::failure(
                            request.id.clone(),
                            -32602,
                            "missing SDK question request ID",
                        ),
                    )?;
                } else if interactions.len() >= RPC_LIMIT {
                    send(
                        &requests,
                        Response::failure(
                            request.id.clone(),
                            -32603,
                            "host interaction capacity reached",
                        ),
                    )?;
                } else {
                    let deadline = Instant::now() + INTERACTION_TIMEOUT;
                    interactions.push(PendingInteraction {
                        id: request.id.clone(),
                        deadline,
                        question: question.filter(|_| !approval).map(str::to_owned),
                    });
                    interaction_deadline = Some(deadline);
                }
                deliver = Some(Inbound {
                    frame: Frame::Request(request),
                    interaction_deadline,
                });
                continue;
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

struct PendingCall {
    method: ClientMethod,
    deadline: Instant,
    reply: oneshot::Sender<Result<Response, String>>,
}

struct PendingInteraction {
    id: JsonRpcId,
    deadline: Instant,
    question: Option<String>,
}

fn send(writer: &mpsc::SyncSender<Frame>, frame: Frame) -> Result<(), String> {
    writer.try_send(frame).map_err(|error| match error {
        mpsc::TrySendError::Full(_) => "SDK stdin queue saturated".into(),
        mpsc::TrySendError::Disconnected(_) => "SDK stdin writer stopped".into(),
    })
}

#[cfg(all(test, unix))]
mod tests;
