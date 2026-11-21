//! Terminal input and official SDK request orchestration.

mod interaction;

use std::collections::HashSet;
use std::future::pending;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::path::PathBuf;
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
    Cwd(bool),
}

enum Overlay {
    Help,
    Commands(Draft),
    History { picker: Picker },
    Inventory(Picker),
}

#[derive(Clone)]
enum PickAction {
    Local(String),
    Official(String),
    Route(String, String),
    Skill(String),
    None,
}

struct Picker {
    title: &'static str,
    rows: Option<Vec<(String, PickAction)>>,
    query: Draft,
    selected: usize,
}

impl Picker {
    fn new(title: &'static str, rows: Option<Vec<(String, PickAction)>>) -> Self {
        Self {
            title,
            rows,
            query: Draft::default(),
            selected: 0,
        }
    }

    fn matches(&self) -> impl Iterator<Item = &(String, PickAction)> {
        let query = self.query.text().to_lowercase();
        self.rows
            .iter()
            .flatten()
            .filter(move |(label, _)| label.to_lowercase().contains(&query))
    }

    fn modal(&self) -> Modal<'_> {
        let body = if self.rows.is_none() {
            "Reading session index…".into()
        } else {
            let start = self.selected.saturating_sub(3);
            let rows = self
                .matches()
                .enumerate()
                .skip(start)
                .take(5)
                .map(|(index, (label, _))| {
                    format!("{} {label}", if index == self.selected { "›" } else { " " })
                })
                .collect::<Vec<_>>()
                .join("\n");
            if rows.is_empty() {
                "No matching entries".into()
            } else {
                rows
            }
        };
        Modal {
            title: self.title,
            body: format!("Type to filter · ↑/↓ select · Enter opens · Esc closes\n{body}"),
            input: Some(&self.query),
        }
    }

    fn key(&mut self, key: KeyEvent) -> Option<PickAction> {
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.matches().count().saturating_sub(1))
            }
            KeyCode::Enter => {
                return self
                    .matches()
                    .nth(self.selected)
                    .map(|(_, action)| action.clone());
            }
            _ => {
                if self.query.text().len() < 256 || !matches!(key.code, KeyCode::Char(_)) {
                    edit_key(&mut self.query, key, false);
                }
                self.selected = 0;
            }
        }
        None
    }
}

type HistoryResult = io::Result<Vec<(String, PickAction)>>;
type StageResult = (String, io::Result<PathBuf>);

fn inventory_rows(kind: &str, inventory: &Value) -> (&'static str, Vec<(String, PickAction)>) {
    let values = |key: &str| inventory[key].as_array().into_iter().flatten();
    let string = |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_owned();
    match kind {
        "models" => {
            let mut rows = Vec::new();
            for provider in values("providers") {
                let id = string(provider, "id");
                let name = string(provider, "name");
                let models = provider["models"].as_array().into_iter().flatten();
                if models.clone().next().is_none() {
                    rows.push((
                        format!("{name} ({id}) · no discoverable models"),
                        PickAction::None,
                    ));
                }
                for model in models {
                    let model = string(model, "id");
                    rows.push((
                        format!("{id} / {model}"),
                        PickAction::Route(id.clone(), model),
                    ));
                }
            }
            (" Provider / Model ", rows)
        }
        "plugins" => (
            " Official Plugins ",
            values("plugins")
                .map(|entry| {
                    let phase = entry["phase"].as_str().unwrap_or("inactive");
                    (
                        format!("{} · {phase}", string(entry, "name")),
                        PickAction::None,
                    )
                })
                .collect(),
        ),
        "skills" => (
            " Official Skills ",
            values("skills")
                .map(|entry| {
                    let name = string(entry, "name");
                    let label = format!("{name} · {}", string(entry, "source"));
                    let action = if entry["userInvocable"].as_bool() == Some(true) {
                        PickAction::Skill(name)
                    } else {
                        PickAction::None
                    };
                    (label, action)
                })
                .collect(),
        ),
        _ => (
            " Official MCP ",
            values("mcp")
                .map(|entry| {
                    (
                        format!("{} · {}", string(entry, "name"), string(entry, "state")),
                        PickAction::None,
                    )
                })
                .collect(),
        ),
    }
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
    history: Option<tokio::task::JoinHandle<HistoryResult>>,
    staging: Option<tokio::task::JoinHandle<StageResult>>,
    interactions: Vec<Interaction>,
    overlay: Option<Overlay>,
    status: String,
    cwd: String,
    route: String,
    version: String,
    attached: bool,
    opened: HashSet<String>,
    resume_on_ready: bool,
    alt_newline: bool,
    quit: bool,
    archive_error: Option<io::Error>,
    offline: bool,
}

impl App {
    fn new(
        version: String,
        config: Option<Result<RuntimeConfig, String>>,
        conversation: Conversation,
    ) -> Self {
        let mut app = Self {
            transport: TransportState::Detached,
            conversation,
            viewport: Viewport::default(),
            draft: Draft::default(),
            recovered: None,
            session: new_session(),
            viewed: None,
            config: None,
            runtime: None,
            operation: None,
            history: None,
            staging: None,
            interactions: Vec::new(),
            overlay: None,
            status: String::new(),
            cwd: String::new(),
            route: String::new(),
            version,
            attached: false,
            opened: HashSet::new(),
            resume_on_ready: false,
            alt_newline: std::env::var("DSH_TUI_NEWLINE").as_deref() == Ok("alt-enter"),
            quit: false,
            archive_error: None,
            offline: config.is_none(),
        };
        match config {
            Some(Ok(config)) => app.start(config),
            Some(Err(error)) => app.transport = TransportState::Failed(error),
            None => app.route = "Offline replay".into(),
        }
        app
    }

    fn start(&mut self, config: RuntimeConfig) {
        self.opened.clear();
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
                body: if self.offline {
                    "/failure   /help   /quit\nOffline: Agent and tools are disabled."
                } else {
                    "/models   /history   /plugins   /skills   /mcp\n/new   /resume ID   /cwd   /route PROVIDER MODEL\n/reload   /view ID   /back   /failure   /draft   /help   /quit"
                }.into(),
                input: Some(input),
            }),
            Some(Overlay::History { picker } | Overlay::Inventory(picker)) => Some(picker.modal()),
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
        let started = Instant::now();
        let result = terminal
            .draw(|frame| screen::draw(frame, &screen, &self.conversation, &mut self.viewport));
        tracing::debug!(
            draw_us = started.elapsed().as_micros() as u64,
            "terminal draw"
        );
        result
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
        if self.offline {
            self.status = "Offline replay · prompts are disabled".into();
            return;
        }
        if self.draft.text().trim().is_empty() || self.busy() || self.staging.is_some() {
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
                self.opened.insert(self.session.clone());
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
                let flushed = self
                    .conversation
                    .flush()
                    .and_then(|()| self.conversation.sync());
                self.archive_result(flushed);
                self.status = "Turn finished".into();
            }
            (Operation::Wait, Err(error)) => self.status = error,
            (Operation::Resume(id), Ok(_)) => self.select_session(id),
            (Operation::Cwd(verbose), Ok(result)) => {
                if let Some(cwd) = result["cwd"].as_str() {
                    self.cwd = cwd.into();
                    self.attached = true;
                    self.opened.insert(self.session.clone());
                    if verbose {
                        self.status = format!("Session directory · {cwd}");
                    } else {
                        self.status = "Session resumed".into();
                    }
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
            let settled = self.conversation.disconnect();
            self.archive_result(settled);
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
                let recorded = self.conversation.push(notification);
                self.archive_result(recorded);
            }
            Frame::Request(request) => {
                let recorded = self.conversation.request(request.clone());
                if recorded.is_err() {
                    self.archive_result(recorded);
                    return;
                }
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
            }
            Frame::Response(_) => {}
        }
    }

    fn archive_result(&mut self, result: io::Result<()>) {
        if let Err(error) = result {
            self.archive_error.get_or_insert_with(|| {
                io::Error::new(error.kind(), format!("Host archive failed: {error}"))
            });
            self.quit = true;
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
        let settled = self.conversation.disconnect();
        self.archive_result(settled);
        if self.quit {
            return;
        }
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
        if let Err(error) = self.conversation.replay(&id) {
            self.status = format!("History replay failed: {error}");
            return;
        }
        if self.opened.contains(&id) {
            self.select_session(id);
            return;
        }
        if let Err(error) = self.call(
            Operation::Resume(id.clone()),
            ClientMethod::SessionResume,
            session_params(&id),
            REQUEST_TIMEOUT,
        ) {
            self.status = error;
        }
    }

    fn select_session(&mut self, id: String) {
        self.opened.insert(id.clone());
        self.session = id;
        self.attached = true;
        self.viewed = None;
        self.viewport.reset();
        self.status = "Reading session directory…".into();
        if let Err(error) = self.call(
            Operation::Cwd(false),
            ClientMethod::SessionWorkingDirectoryGet,
            session_params(&self.session),
            REQUEST_TIMEOUT,
        ) {
            self.status = error;
        }
    }

    fn open_history(&mut self) {
        let ecosystem = self
            .config
            .as_ref()
            .and_then(RuntimeConfig::ecosystem)
            .cloned();
        let local = self.conversation.history();
        let title = if ecosystem.is_some() {
            " DSH History "
        } else {
            " Local Host History "
        };
        self.overlay = Some(Overlay::History {
            picker: Picker::new(title, None),
        });
        if self.history.is_some() {
            return;
        }
        self.history = Some(tokio::task::spawn_blocking(move || {
            let mut rows = Vec::new();
            if let Some(ecosystem) = ecosystem {
                for session in ecosystem.history()? {
                    let label = format!(
                        "{}  {}  {}{}",
                        session.date,
                        session.id,
                        session.cwd,
                        if session.current {
                            ""
                        } else {
                            " · older format"
                        }
                    );
                    let action = if session.current {
                        PickAction::Official(session.id)
                    } else {
                        PickAction::None
                    };
                    rows.push((label, action));
                }
            }
            for id in local?.sessions()? {
                if !rows.iter().any(
                    |(_, action)| matches!(action, PickAction::Official(found) if *found == id),
                ) {
                    rows.push((format!("{id}  · TUI"), PickAction::Local(id)));
                }
            }
            Ok(rows)
        }));
    }

    fn open_inventory(&mut self, kind: &str) {
        let Some(config) = &self.config else { return };
        let Some(ecosystem) = config.ecosystem() else {
            self.status = "Set DSH_TUI_DSH_HOME to use the official inventory".into();
            return;
        };
        let Ok(inventory) = ecosystem.inventory() else {
            self.status =
                "Official inventory is loading or unavailable · retry after the SDK is ready"
                    .into();
            return;
        };
        let (title, mut rows) = inventory_rows(kind, &inventory);
        if kind == "models" && !rows.iter().any(|(_, action)| matches!(action, PickAction::Route(p, m) if *p == config.provider && *m == config.model)) {
            rows.insert(0, (format!("{} / {} · current", config.provider, config.model), PickAction::Route(config.provider.clone(), config.model.clone())));
        }
        self.overlay = Some(Overlay::Inventory(Picker::new(title, Some(rows))));
    }

    async fn pick(&mut self, action: PickAction) {
        if matches!(action, PickAction::None) {
            return;
        }
        if self.busy() || self.operation.is_some() || self.staging.is_some() {
            self.status = "Finish the active request first".into();
            return;
        }
        self.overlay = None;
        match action {
            PickAction::Local(id) => self.resume(id),
            PickAction::Official(id) => {
                if let Some(ecosystem) = self
                    .config
                    .as_ref()
                    .and_then(RuntimeConfig::ecosystem)
                    .cloned()
                {
                    self.status = "Reading official session snapshot…".into();
                    self.staging = Some(tokio::task::spawn_blocking(move || {
                        let result = ecosystem.stage(&id);
                        (id, result)
                    }));
                }
            }
            PickAction::Route(provider, model) => {
                if let Some(mut config) = self.config.clone() {
                    if config.provider != provider || config.model != model {
                        config.provider = provider;
                        config.model = model;
                        self.restart(config, false).await;
                    }
                }
            }
            PickAction::Skill(name) => {
                self.viewport.focused = false;
                self.draft.replace(format!("/{name} "));
            }
            PickAction::None => (),
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
            _ if self.offline => self.status = "Unavailable in offline replay".into(),
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
            ["/new" | "/history" | "/resume" | "/route" | "/reload", ..]
                if self.busy() || self.operation.is_some() || self.staging.is_some() =>
            {
                self.status = "Finish or interrupt the active request first".into();
            }
            ["/new"] => {
                self.session = new_session();
                self.attached = false;
                if let Some(config) = &self.config {
                    self.cwd = config.cwd.to_string_lossy().into_owned();
                }
                self.viewed = None;
                self.viewport.reset();
                self.status = "New session".into();
            }
            ["/resume", id] => self.resume((*id).into()),
            ["/history"] => self.open_history(),
            [kind @ ("/models" | "/plugins" | "/skills" | "/mcp")] => {
                self.open_inventory(&kind[1..])
            }
            ["/cwd"] => {
                if let Err(error) = self.call(
                    Operation::Cwd(true),
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
            ["/reload"] => {
                if let Some(config) = self.config.clone() {
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
            } else if let Overlay::History { picker } | Overlay::Inventory(picker) = overlay {
                if let Some(action) = picker.key(key) {
                    self.pick(action).await;
                }
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

pub async fn run(state: AppState, replay: Option<PathBuf>, show_history: bool) -> io::Result<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let name = format!("{:032}-{}", stamp.as_nanos(), new_session());
    let root = crate::build_info::workspace_root();
    let mut app = if let Some(path) = replay {
        let started = Instant::now();
        let (conversation, id, events) =
            Conversation::open_replay(&root.join(".dsh-tui/replay"), &name, &path)?;
        tracing::debug!(
            events,
            replay_us = started.elapsed().as_micros() as u64,
            "offline replay loaded"
        );
        let mut app = App::new(state.agent_version, None, conversation);
        app.session = id;
        app.status = format!("Offline replay · {events} recorded events");
        app
    } else {
        let conversation = Conversation::open(&root.join(".dsh-tui/history"), &name)?;
        App::new(
            state.agent_version,
            Some(RuntimeConfig::load(&root)),
            conversation,
        )
    };
    if show_history {
        app.open_history();
    }
    let mut terminal = TerminalSession::enter()?;
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
    let synced = app.conversation.sync();
    result
        .and(restored)
        .and(app.archive_error.map_or(Ok(()), Err))
        .and(synced)
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
            history = async {
                match &mut app.history {
                    Some(task) => task.await.map_err(io::Error::other).and_then(|result| result),
                    None => pending().await,
                }
            } => {
                app.history = None;
                if let Some(Overlay::History { picker }) = &mut app.overlay {
                    match history {
                        Ok(found) => picker.rows = Some(found),
                        Err(error) => {
                            app.overlay = None;
                            app.status = format!("History scan failed: {error}");
                        }
                    }
                }
            },
            staged = async {
                match &mut app.staging {
                    Some(task) => task.await,
                    None => pending().await,
                }
            } => {
                app.staging = None;
                match staged {
                    Ok((id, Ok(path))) => {
                        let imported = app.conversation.import(&id, &path);
                        let _ = std::fs::remove_file(path);
                        if let Err(error) = imported {
                            app.status = format!("Official history import failed: {error}");
                        } else {
                            app.resume(id);
                        }
                    }
                    Ok((_, Err(error))) => app.status = format!("Official history unavailable: {error}"),
                    Err(_) => app.status = "Official history worker stopped".into(),
                }
            },
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
