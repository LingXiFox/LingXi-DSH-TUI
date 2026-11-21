//! Terminal input and official SDK request orchestration.

mod interaction;

use std::future::pending;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use dsh_protocol::protocol::{ClientMethod, Frame, Response, ServerNotification};
use dsh_protocol::stdio::{prompt_params, session_params};
use dsh_state::conversation::Conversation;
use dsh_state::draft::{Draft, Motion};
use dsh_state::{AppState, TransportState};
use serde_json::Value;
use tokio::sync::mpsc;

use self::interaction::Interaction;
use crate::render::{
    chat::Viewport,
    screen::{self, Modal, Screen},
};
use crate::runtime::{Inbound, RpcResult, RuntimeConfig, RuntimeHandle};
use crate::terminal::TerminalSession;

const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const TURN_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const DRAFT_LIMIT: usize = 32 * 1024;

enum Operation {
    Prompt(String),
    Wait,
    Resume(String),
    Cwd,
}

enum Overlay {
    Help,
    Commands(Draft),
}

struct App {
    transport: TransportState,
    conversation: Conversation,
    viewport: Viewport,
    draft: Draft,
    recovered: Option<String>,
    session: String,
    viewed: Option<String>,
    config: Option<RuntimeConfig>,
    runtime: Option<RuntimeHandle>,
    operation: Option<(Operation, RpcResult)>,
    interactions: Vec<Interaction>,
    overlay: Option<Overlay>,
    status: String,
    cwd: String,
    route: String,
    version: String,
    attached: bool,
    resume_on_ready: bool,
    alt_newline: bool,
    quit: bool,
}

impl App {
    fn new(version: String, config: Result<RuntimeConfig, String>) -> Self {
        let mut app = Self {
            transport: TransportState::Detached,
            conversation: Conversation::default(),
            viewport: Viewport::default(),
            draft: Draft::default(),
            recovered: None,
            session: new_session(),
            viewed: None,
            config: None,
            runtime: None,
            operation: None,
            interactions: Vec::new(),
            overlay: None,
            status: String::new(),
            cwd: String::new(),
            route: String::new(),
            version,
            attached: false,
            resume_on_ready: false,
            alt_newline: std::env::var("DSH_TUI_NEWLINE").as_deref() == Ok("alt-enter"),
            quit: false,
        };
        match config {
            Ok(config) => app.start(config),
            Err(error) => app.transport = TransportState::Failed(error),
        }
        app
    }

    fn start(&mut self, config: RuntimeConfig) {
        self.cwd = config.cwd.to_string_lossy().into_owned();
        self.route = format!("{} / {}", config.provider, config.model);
        self.config = Some(config.clone());
        match RuntimeHandle::start(config) {
            Ok(handle) => {
                self.runtime = Some(handle);
                self.transport = TransportState::Connecting;
            }
            Err(error) => self.transport = TransportState::Failed(error.to_string()),
        }
    }

    fn id(&self) -> &str {
        self.viewed.as_deref().unwrap_or(&self.session)
    }

    fn busy(&self) -> bool {
        matches!(
            self.operation,
            Some((Operation::Prompt(_) | Operation::Wait, _))
        ) || self
            .conversation
            .sessions
            .get(&self.session)
            .is_some_and(|session| session.running)
    }

    fn draw(&mut self, terminal: &mut TerminalSession) -> io::Result<()> {
        let running = self.busy();
        let id = self.viewed.as_deref().unwrap_or(&self.session);
        let modal = self.interactions.first().map(Interaction::modal).or_else(|| match &self.overlay {
            Some(Overlay::Help) => Some(Modal {
                title: " Keyboard ",
                body: "Enter sends · Ctrl+J inserts a newline\nShift+arrows select · Ctrl+A selects all\nPgUp/PgDn scroll · F6 switches focus\nTab folds · Ctrl+L changes mode · Ctrl+P opens commands\nEsc returns · Ctrl+C interrupts or quits".into(),
                input: None,
            }),
            Some(Overlay::Commands(input)) => Some(Modal {
                title: " Commands ",
                body: "/new   /resume ID   /cwd   /route PROVIDER MODEL\n/view ID   /back   /failure   /draft   /help   /quit\nType a command and press Enter.".into(),
                input: Some(input),
            }),
            None => None,
        });
        let screen = Screen {
            cwd: &self.cwd,
            session: id,
            route: &self.route,
            version: &self.version,
            transport: &self.transport,
            running,
            status: &self.status,
            draft: &self.draft,
            modal,
        };
        terminal.draw(|frame| screen::draw(frame, &screen, &self.conversation, &mut self.viewport))
    }

    fn call(
        &mut self,
        operation: Operation,
        method: ClientMethod,
        params: Value,
        timeout: Duration,
    ) -> Result<(), String> {
        if self.operation.is_some() {
            return Err("An SDK request is already in progress".into());
        }
        if !matches!(self.transport, TransportState::Ready { .. }) {
            return Err("SDK is not ready".into());
        }
        let reply = self
            .runtime
            .as_ref()
            .ok_or("SDK is disconnected")?
            .call(method, params, timeout)?;
        self.operation = Some((operation, reply));
        Ok(())
    }

    fn submit(&mut self) {
        if self.draft.text().trim().is_empty() || self.busy() {
            return;
        }
        let prompt = self.draft.text().to_owned();
        let params = prompt_params(&self.session, &prompt);
        match self.call(
            Operation::Prompt(prompt),
            ClientMethod::SessionPrompt,
            params,
            REQUEST_TIMEOUT,
        ) {
            Ok(()) => {
                self.draft = Draft::default();
                self.viewed = None;
                self.viewport.reset();
                self.status = "Sending…".into();
            }
            Err(error) => self.status = error,
        }
    }

    fn recover(&mut self, prompt: String) {
        if self.draft.text().is_empty() {
            self.draft.replace(prompt);
        } else {
            self.recovered = Some(prompt);
        }
    }

    fn reply(&mut self, result: Result<Response, String>) {
        let Some((operation, _)) = self.operation.take() else {
            return;
        };
        let result = result.and_then(Response::decode_result::<Value>);
        match (operation, result) {
            (Operation::Prompt(_), Ok(result)) if result["messageId"].is_string() => {
                self.attached = true;
                self.status = "Agent running".into();
                if let Err(error) = self.call(
                    Operation::Wait,
                    ClientMethod::SessionWait,
                    session_params(&self.session),
                    TURN_TIMEOUT,
                ) {
                    self.status = error;
                }
            }
            (Operation::Prompt(prompt), result) => {
                self.recover(prompt);
                self.status = format!(
                    "Draft kept · {}",
                    result
                        .err()
                        .unwrap_or_else(|| "SDK returned no message ID".into())
                );
            }
            (Operation::Wait, Ok(_)) => {
                self.conversation.flush();
                self.status = "Turn finished".into();
            }
            (Operation::Wait, Err(error)) => self.status = error,
            (Operation::Resume(id), Ok(_)) => {
                self.session = id;
                self.attached = true;
                self.viewed = None;
                self.viewport.reset();
                self.status = "Session resumed · SDK supplies new events".into();
            }
            (Operation::Cwd, Ok(result)) => {
                if let Some(cwd) = result["cwd"].as_str() {
                    self.cwd = cwd.into();
                    self.attached = true;
                    self.status = format!("Session directory · {cwd}");
                } else {
                    self.status = "SDK returned no working directory".into();
                }
            }
            (_, Err(error)) => self.status = error,
        }
    }

    fn transport(&mut self, state: TransportState) {
        let ready = matches!(state, TransportState::Ready { .. });
        if let TransportState::Failed(error) = &state {
            // The supervisor publishes failures on a separate channel after the last frame.
            while let Some(frame) = self
                .runtime
                .as_mut()
                .and_then(|handle| handle.inbox.try_recv().ok())
            {
                self.inbound(frame);
            }
            self.status = error.clone();
            self.conversation.disconnect();
            self.interactions.clear();
        }
        self.transport = state;
        if ready && std::mem::take(&mut self.resume_on_ready) {
            self.resume(self.session.clone());
        }
    }

    fn inbound(&mut self, inbound: Inbound) {
        match inbound.frame {
            Frame::Notification(notification) => {
                if notification.method == ServerNotification::UserQuestionCancelled.as_str() {
                    if let Some(request_id) = notification
                        .params
                        .as_ref()
                        .and_then(|p| p["requestId"].as_str())
                    {
                        self.interactions
                            .retain(|item| item.request_id() != Some(request_id));
                    }
                }
                self.conversation.push(notification);
            }
            Frame::Request(request) => {
                if let Some(deadline) = inbound.interaction_deadline {
                    if let Some(interaction) = Interaction::from_request(&request, deadline) {
                        if deadline > Instant::now() {
                            self.interactions.push(interaction);
                        }
                    } else if let Some(runtime) = &self.runtime {
                        let cancelled =
                            if request.method == dsh_protocol::protocol::METHOD_APPROVAL_REQUEST {
                                serde_json::json!({"outcome":"cancelled"})
                            } else {
                                serde_json::json!({"cancelled":true})
                            };
                        let _ = runtime.answer(request.id.clone(), cancelled);
                    }
                }
                self.conversation.request(request);
            }
            Frame::Response(_) => {}
        }
    }

    async fn restart(&mut self, config: RuntimeConfig, interrupt: bool) {
        if let Some((Operation::Prompt(prompt), _)) = self.operation.take() {
            self.recover(prompt);
        }
        if let Some(runtime) = self.runtime.take() {
            if let Err(error) = runtime.shutdown(|frame| self.inbound(frame)).await {
                tracing::warn!(%error, "SDK shutdown used process cleanup");
            }
        }
        self.interactions.clear();
        self.conversation.disconnect();
        self.resume_on_ready = self.attached;
        self.status = if interrupt {
            "Task interrupted · reconnecting"
        } else {
            "Reinitializing SDK route"
        }
        .into();
        self.start(config);
    }

    fn resume(&mut self, id: String) {
        if let Err(error) = self.call(
            Operation::Resume(id.clone()),
            ClientMethod::SessionResume,
            session_params(&id),
            REQUEST_TIMEOUT,
        ) {
            self.status = error;
        }
    }

    async fn command(&mut self, input: &str) {
        let words: Vec<_> = input.split_whitespace().collect();
        match words.as_slice() {
            ["/quit"] => self.quit = true,
            ["/help"] => self.overlay = Some(Overlay::Help),
            ["/failure"] => {
                let id = self.id().to_owned();
                if !self.viewport.failure(&id, &self.conversation) {
                    self.status = "No failed tool in this view".into();
                }
            }
            ["/draft"] if self.draft.text().is_empty() => {
                if let Some(text) = self.recovered.take() {
                    self.draft.replace(text);
                }
            }
            ["/view", id] if self.conversation.sessions.contains_key(*id) => {
                self.viewed = Some((*id).into());
                self.viewport.reset();
            }
            ["/back"] => {
                self.viewed = None;
                self.viewport.reset();
            }
            ["/new" | "/resume" | "/route", ..] if self.busy() || self.operation.is_some() => {
                self.status = "Finish or interrupt the active request first".into();
            }
            ["/new"] => {
                self.session = new_session();
                self.attached = false;
                self.viewed = None;
                self.viewport.reset();
                self.status = "New session".into();
            }
            ["/resume", id] => self.resume((*id).into()),
            ["/cwd"] => {
                if let Err(error) = self.call(
                    Operation::Cwd,
                    ClientMethod::SessionWorkingDirectoryGet,
                    session_params(&self.session),
                    REQUEST_TIMEOUT,
                ) {
                    self.status = error;
                }
            }
            ["/route", provider, model] => {
                if let Some(mut config) = self.config.clone() {
                    config.provider = (*provider).into();
                    config.model = (*model).into();
                    self.restart(config, false).await;
                }
            }
            _ => self.status = "Unknown or unavailable command · Ctrl+P lists commands".into(),
        }
    }

    async fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(interaction) = self.interactions.first_mut() {
            if let Some(result) = interaction.key(key) {
                if let Some(runtime) = &self.runtime {
                    match runtime.answer(interaction.id().clone(), result) {
                        Ok(()) => {
                            self.interactions.remove(0);
                        }
                        Err(error) => self.status = error,
                    }
                }
            }
            return;
        }
        if let Some(overlay) = &mut self.overlay {
            if key.code == KeyCode::Esc || ctrl && key.code == KeyCode::Char('c') {
                self.overlay = None;
            } else if let Overlay::Commands(draft) = overlay {
                if key.code == KeyCode::Enter {
                    let command = draft.text().to_owned();
                    self.overlay = None;
                    self.command(&command).await;
                } else {
                    edit_key(draft, key, false);
                }
            }
            return;
        }
        if ctrl && key.code == KeyCode::Char('c') {
            if self.busy() {
                if let Some(config) = self.config.clone() {
                    self.restart(config, true).await;
                }
            } else {
                self.quit = true;
            }
        } else if ctrl && key.code == KeyCode::Char('p') {
            self.overlay = Some(Overlay::Commands(Draft::default()));
        } else if ctrl && key.code == KeyCode::Char('l') {
            self.viewport.mode = self.viewport.mode.next();
        } else if key.code == KeyCode::F(6) {
            self.viewport.focused = !self.viewport.focused;
        } else if key.code == KeyCode::Esc {
            if self.viewed.take().is_some() {
                self.viewport.reset();
            }
            self.viewport.focused = false;
        } else if key.code == KeyCode::Char('?')
            && (self.viewport.focused || self.draft.text().is_empty())
        {
            self.overlay = Some(Overlay::Help);
        } else if key.code == KeyCode::Tab {
            let id = self.id().to_owned();
            self.viewport.toggle(&id, &self.conversation);
        } else if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
            || self.viewport.focused && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            let id = self.id().to_owned();
            self.viewport.focused = true;
            self.viewport.scroll(
                &id,
                &self.conversation,
                matches!(key.code, KeyCode::PageDown | KeyCode::Down),
                matches!(key.code, KeyCode::PageUp | KeyCode::PageDown),
            );
        } else if self.viewport.focused {
            match key.code {
                KeyCode::Home => self.viewport.home(),
                KeyCode::End => self.viewport.reset(),
                _ => {}
            }
        } else if !edit_key(&mut self.draft, key, self.alt_newline) && key.code == KeyCode::Enter {
            self.submit();
        }
    }

    fn paste(&mut self, text: &str) {
        if let Some(interaction) = self.interactions.first_mut() {
            interaction.paste(text);
        } else if let Some(Overlay::Commands(draft)) = &mut self.overlay {
            insert(draft, text);
        } else if self.overlay.is_none() && !self.viewport.focused {
            insert(&mut self.draft, text);
        }
    }
}

fn new_session() -> String {
    let random = RandomState::new();
    format!(
        "session-{:016x}{:016x}",
        random.hash_one(0),
        random.hash_one(1)
    )
}

fn insert(draft: &mut Draft, text: &str) {
    let available = DRAFT_LIMIT.saturating_sub(draft.text().len() - draft.selection().len());
    let sanitized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut output = String::new();
    for ch in sanitized
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
    {
        if output.len() + ch.len_utf8() > available {
            break;
        }
        output.push(ch);
    }
    if !output.is_empty() {
        draft.insert(&output);
    }
}

fn edit_key(draft: &mut Draft, key: KeyEvent, alt_newline: bool) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('a') if ctrl => draft.select_all(),
        KeyCode::Char('j') if ctrl => insert(draft, "\n"),
        KeyCode::Enter if shift || alt && alt_newline => insert(draft, "\n"),
        KeyCode::Char(ch) if !ctrl && !alt => insert(draft, ch.encode_utf8(&mut [0; 4])),
        KeyCode::Backspace => draft.delete(true),
        KeyCode::Delete => draft.delete(false),
        KeyCode::Left => draft.move_to(Motion::Left, shift),
        KeyCode::Right => draft.move_to(Motion::Right, shift),
        KeyCode::Up => draft.move_to(Motion::Up, shift),
        KeyCode::Down => draft.move_to(Motion::Down, shift),
        KeyCode::Home => draft.move_to(Motion::Home, shift),
        KeyCode::End => draft.move_to(Motion::End, shift),
        _ => return false,
    }
    true
}

pub async fn run(state: AppState, config: Result<RuntimeConfig, String>) -> io::Result<()> {
    let mut terminal = TerminalSession::enter()?;
    let mut app = App::new(state.agent_version, config);
    let (sender, receiver) = mpsc::channel(64);
    let reader = std::thread::Builder::new()
        .name("dsh-tui-input".into())
        .spawn(move || {
            while !sender.is_closed() {
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => match event::read() {
                        Ok(event) if !sender.is_closed() => {
                            if sender.blocking_send(event).is_err() {
                                break;
                            }
                        }
                        _ => break,
                    },
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(%error, "terminal input stopped");
                        break;
                    }
                }
            }
        })?;
    let result = event_loop(&mut terminal, &mut app, receiver).await;
    let restored = terminal.restore();
    let _ = reader.join();
    if let Some(runtime) = app.runtime.take() {
        if let Err(error) = runtime.shutdown(|frame| app.inbound(frame)).await {
            tracing::warn!(%error, "SDK shutdown used process cleanup");
        }
    }
    result.and(restored)
}

enum SdkEvent {
    State(TransportState),
    Frame(Inbound),
}

async fn sdk_event(handle: Option<&mut RuntimeHandle>) -> SdkEvent {
    let Some(handle) = handle else {
        return pending().await;
    };
    tokio::select! {
        Some(state) = handle.events.recv() => SdkEvent::State(state),
        Some(frame) = handle.inbox.recv() => SdkEvent::Frame(frame),
        else => pending().await,
    }
}

async fn event_loop(
    terminal: &mut TerminalSession,
    app: &mut App,
    mut input: mpsc::Receiver<Event>,
) -> io::Result<()> {
    app.draw(terminal)?;
    let mut next_frame = tokio::time::Instant::now() + FRAME_INTERVAL;
    let mut dirty = false;
    while !app.quit {
        let expires = app.interactions.first().map(Interaction::expires);
        tokio::select! {
            event = input.recv() => match event {
                Some(Event::Key(key)) => app.key(key).await,
                Some(Event::Paste(text)) => app.paste(&text),
                Some(_) => {},
                None => break,
            },
            event = sdk_event(app.runtime.as_mut()) => match event {
                SdkEvent::State(state) => app.transport(state),
                SdkEvent::Frame(frame) => app.inbound(frame),
            },
            reply = async {
                match &mut app.operation {
                    Some((_, reply)) => reply.await.unwrap_or_else(|_| Err("SDK connection lost · request outcome unknown".into())),
                    None => pending().await,
                }
            } => app.reply(reply),
            _ = async {
                match expires {
                    Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                    None => pending().await,
                }
            } => {
                app.interactions.retain(|item| item.expires() > Instant::now());
                app.status = "Host interaction expired".into();
            },
            _ = tokio::time::sleep_until(next_frame), if dirty => {
                app.draw(terminal)?;
                next_frame = tokio::time::Instant::now() + FRAME_INTERVAL;
                dirty = false;
                continue;
            }
        }
        dirty = true;
    }
    Ok(())
}
