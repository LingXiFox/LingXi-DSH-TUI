//! Ordered SDK event journal and turn-based transcript projection.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dsh_protocol::protocol::{Frame, Notification, Request, ServerNotification};
use serde_json::Value;

pub use crate::archive::Text;
use crate::archive::{Archive, Record};

const REORDER_WINDOW: usize = 32;

/// Display rows identified by their index inside a session.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A message or human-readable status with an archived body.
    Text {
        /// Semantic presentation category.
        kind: TextKind,
        /// Terminal-independent text or Markdown.
        body: Text,
    },
    /// Consecutive tool activity, keyed by the first event sequence.
    ToolActivity(u64),
    /// In-process child session linked by the official notification.
    SubAgent(String),
}

/// Text presentation categories; never inferred from labels in the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    /// A user-authored prompt.
    UserMessage,
    /// An assistant response.
    AssistantMessage,
    /// Recorded model reasoning.
    Reasoning,
    /// A recorded checklist update.
    Todo,
    /// A permission decision recorded by the Agent.
    Approval,
    /// A user-visible command or turn status.
    Notice,
    /// A user-visible failure.
    Error,
}

/// Current checklist item, as reported by DSH.
#[derive(Debug)]
pub struct Todo {
    /// Checklist label.
    pub content: Text,
    /// Official checklist status.
    pub status: TodoStatus,
}

/// Checklist state, as reported by DSH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    /// Not yet started.
    Pending,
    /// In progress.
    InProgress,
    /// Finished.
    Completed,
}

/// Tool details belonging to an explicit upstream turn and call ID.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tool {
    /// SDK call identity.
    pub id: String,
    /// SDK tool name, if its call record is available.
    pub name: String,
    /// The step that issued this call within its turn.
    pub step: Option<u64>,
    /// Original arguments string.
    pub arguments: Text,
    /// Path explicitly named by a supported filesystem tool.
    pub path: Option<String>,
    /// Text projection of the tool's result blocks.
    pub output: Option<Text>,
    /// SDK-reported failure.
    pub failed: bool,
    /// SDK-reported user-friendly error, when present.
    pub error: Option<Text>,
    /// The runtime disconnected before a result was observed.
    pub interrupted: bool,
}

/// A contiguous group of calls belonging to one upstream turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Turn {
    /// Upstream turn number.
    pub number: u64,
    /// Calls ordered by their first recorded event.
    pub tools: Vec<Tool>,
    /// Tool name counts for the compact view.
    pub counts: BTreeMap<String, usize>,
    /// Running calls in the turn.
    pub running: usize,
    /// Failed calls in the turn.
    pub failed: usize,
    /// Calls interrupted by disconnection without result.
    pub interrupted: usize,
}

/// Model metadata recorded in a model selection or resolved request header.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelRoute {
    /// Provider ID.
    pub provider: String,
    /// Model ID within that provider.
    pub model: String,
    /// SDK-reported effort when available.
    pub reasoning_effort: Option<String>,
}

impl ModelRoute {
    fn from_event(data: &Value) -> Option<Self> {
        Some(Self {
            provider: data["provider"].as_str()?.into(),
            model: data["model"].as_str()?.into(),
            reasoning_effort: data["reasoningEffort"].as_str().map(str::to_owned),
        })
    }
}

/// Optional cache counters from one assistant response. Absence differs from zero.
#[derive(Debug, PartialEq, Eq)]
pub struct CacheUsage {
    /// Cached input tokens read.
    pub read: Option<u64>,
    /// Input tokens written to the cache.
    pub write: Option<u64>,
}

/// An SDK session projected independently of every other session.
#[derive(Debug, Default)]
pub struct Session {
    /// Renderable top-level rows.
    pub rows: Vec<Row>,
    /// Tool activity groups keyed by their first event sequence.
    pub activity: BTreeMap<u64, Turn>,
    /// Whole-agent status from the SDK.
    pub running: bool,
    /// Parent session if this is a local child.
    pub parent: Option<String>,
    /// Latest recorded model route.
    pub route: Option<ModelRoute>,
    /// Cache counters, if reported for the latest assistant response.
    pub cache: Option<CacheUsage>,
    /// Latest official session title, when recorded.
    pub title: Option<String>,
    /// Input tokens reported by the latest assistant response.
    pub input_tokens: Option<u64>,
    /// Output tokens reported by the latest assistant response.
    pub output_tokens: Option<u64>,
    /// Context capacity advertised by the active model route.
    pub context_window: Option<u64>,
    /// Current checklist items (only the latest snapshot).
    pub todos: Vec<Todo>,
    /// Last reported planning mode.
    pub plan: Option<bool>,
    /// Paths explicitly named by successful edit/write calls, not a Git diff.
    pub edited_paths: BTreeSet<String>,
    /// Latest child completion or turn outcome.
    pub outcome: Option<String>,
    /// Latest assembled assistant text for child-session previews.
    pub summary: Option<Text>,
    /// Row indexes with failed tool calls, for direct navigation.
    pub failures: BTreeMap<usize, usize>,
    /// Number of events whose meaning is not projected by this client.
    pub unhandled: usize,
    /// Direct SDK child session IDs.
    pub children: Vec<String>,
    /// Increments when rows are reconstructed after a late event.
    pub generation: u64,
    ordered: BTreeMap<u64, Record>,
    pending: BTreeMap<u64, Record>,
    last_seq: Option<u64>,
    group_rows: HashMap<u64, usize>,
    calls: HashMap<u64, HashMap<String, (u64, usize)>>,
    approvals: HashMap<String, (usize, String)>,
}

/// Raw SDK messages and their derived session views.
#[derive(Debug, Default)]
pub struct Conversation {
    archive: Archive,
    /// Projections keyed by SDK session ID.
    pub sessions: HashMap<String, Session>,
}

/// A snapshot of the journals to enumerate, without borrowing the live projection.
pub struct History(Vec<PathBuf>);

impl History {
    /// Enumerate root session IDs and recorded titles, newest first.
    pub fn sessions(self) -> io::Result<Vec<(String, Option<String>)>> {
        let mut sessions = HashMap::<String, ((usize, u64), Option<String>)>::new();
        let mut children = HashSet::new();
        for (run, path) in self.0.iter().enumerate() {
            scan(path, |offset, notification| {
                if let Some(id) = session_id(notification) {
                    let entry = sessions.entry(id.to_owned()).or_default();
                    entry.0 = (run, offset);
                    if let Some(event) = notification
                        .params
                        .as_ref()
                        .map(|params| &params["event"])
                        .filter(|event| event["type"] == "session/title")
                    {
                        entry.1 = event["data"]["title"]
                            .as_str()
                            .map(|title| title.chars().take(180).collect());
                    }
                }
                if let Some((_, child)) = child_link(notification) {
                    children.insert(child.to_owned());
                }
                Ok(())
            })?;
        }
        let mut sessions: Vec<_> = sessions.into_iter().collect();
        sessions.retain(|(id, _)| !children.contains(id));
        sessions.sort_unstable_by_key(|(_, (position, _))| std::cmp::Reverse(*position));
        Ok(sessions
            .into_iter()
            .map(|(id, (_, title))| (id, title))
            .collect())
    }
}

impl Conversation {
    /// Create a fresh, private archive. Earlier archives can be replayed by session.
    pub fn open(directory: &Path, name: &str) -> io::Result<Self> {
        Ok(Self {
            archive: Archive::open(directory, name, true)?,
            sessions: HashMap::new(),
        })
    }

    /// Read a single exported DSH session without adding it to live Host history.
    pub fn open_replay(
        directory: &Path,
        name: &str,
        path: &Path,
    ) -> io::Result<(Self, String, usize)> {
        if !path.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected an event journal file",
            ));
        }
        let mut conversation = Self {
            archive: Archive::open(directory, name, false)?,
            sessions: HashMap::new(),
        };
        let source = conversation.archive.source(path)?;
        let mut id = None;
        let mut events = 0;
        scan(path, |offset, notification| {
            let Some(session) = session_id(notification) else {
                return Ok(());
            };
            if id.get_or_insert_with(|| session.to_owned()) != session {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "expected one exported session",
                ));
            }
            events += 1;
            conversation.project(notification, Record { source, offset })
        })?;
        conversation.disconnect()?;
        let id =
            id.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no session events"))?;
        Ok((conversation, id, events))
    }

    /// Count original inbound messages in the current run.
    pub fn received_count(&self) -> usize {
        self.archive.count()
    }

    /// Read a visible body through the bounded transcript cache.
    pub fn body(&self, text: &Text) -> io::Result<Arc<String>> {
        self.archive.body(text)
    }

    /// Load one original event for the explicit Debug Viewer, not the transcript.
    pub fn debug_event(
        &mut self,
        id: &str,
        from: Option<u64>,
        newer: bool,
    ) -> io::Result<Option<(u64, Value)>> {
        let Some(session) = self.sessions.get(id) else {
            return Ok(None);
        };
        let record = match from {
            Some(seq) if newer => session
                .ordered
                .range((std::ops::Bound::Excluded(seq), std::ops::Bound::Unbounded))
                .next(),
            Some(seq) => session.ordered.range(..seq).next_back(),
            None => session.ordered.last_key_value(),
        };
        record
            .map(|(&seq, &record)| Ok((seq, event_at(&mut self.archive, record)?)))
            .transpose()
    }

    /// Ensure a clean exit persists the original inbound records.
    pub fn sync(&self) -> io::Result<()> {
        self.archive.sync()
    }

    /// Record a host request without conflating it with a session-log event.
    pub fn request(&mut self, request: Request) -> io::Result<()> {
        self.archive.append(&Frame::Request(request)).map(|_| ())
    }

    /// Journal first, then project recognized notifications.
    pub fn push(&mut self, notification: Notification) -> io::Result<()> {
        let frame = Frame::Notification(notification);
        let record = self.archive.append(&frame)?;
        if let Frame::Notification(notification) = frame {
            self.project(&notification, record)?;
        }
        Ok(())
    }

    /// Load a detached official session snapshot into the live transcript.
    pub fn import(&mut self, id: &str, path: &Path) -> io::Result<()> {
        if self.sessions.contains_key(id) {
            return Ok(());
        }
        scan(path, |_, event| {
            if session_id(event) != Some(id) {
                return Err(io::Error::other("unexpected session in snapshot"));
            }
            self.push(event.clone())
        })?;
        self.flush()
    }

    fn project(&mut self, notification: &Notification, record: Record) -> io::Result<()> {
        let Some(params) = &notification.params else {
            return Ok(());
        };
        let method = notification.method.as_str();
        if method == ServerNotification::SessionEvent.as_str() {
            let (Some(id), Some(seq)) = (
                params["sessionId"].as_str(),
                params["event"]["seq"].as_u64(),
            ) else {
                return Ok(());
            };
            self.sessions.entry(id.into()).or_default().queue(
                seq,
                record,
                &params["event"],
                &mut self.archive,
            )?;
        } else if method == ServerNotification::SessionStatus.as_str() {
            let Some(id) = params["sessionId"].as_str() else {
                return Ok(());
            };
            let session = self.sessions.entry(id.into()).or_default();
            match params["status"].as_str() {
                Some("idle") => {
                    session.running = false;
                    session.flush(&mut self.archive)?;
                }
                Some("running") => session.running = true,
                _ => {}
            }
        } else if method == ServerNotification::SubagentStarted.as_str()
            || method == ServerNotification::SubagentFinished.as_str()
        {
            let Some((parent, child)) = child_link(notification) else {
                return Ok(());
            };
            let parent_session = self.sessions.entry(parent.into()).or_default();
            if !parent_session.children.iter().any(|id| id == child) {
                parent_session.children.push(child.into());
                parent_session.rows.push(Row::SubAgent(child.into()));
            }
            let session = self.sessions.entry(child.into()).or_default();
            session.parent = Some(parent.into());
            if method == ServerNotification::SubagentFinished.as_str() {
                session.running = false;
                session.outcome = params["status"].as_str().map(str::to_owned);
                session.flush(&mut self.archive)?;
            } else {
                session.running = true;
            }
        }
        Ok(())
    }

    /// Snapshot the archive paths so history enumeration can run off the UI thread.
    pub fn history(&self) -> io::Result<History> {
        self.archive.paths(true).map(History)
    }

    /// Backfill a session captured by an earlier TUI launch using original records.
    pub fn replay(&mut self, id: &str) -> io::Result<()> {
        if self.sessions.contains_key(id) {
            return Ok(());
        }
        let paths = self.archive.paths(false)?;
        let mut children: HashMap<String, HashSet<String>> = HashMap::new();
        for path in &paths {
            scan(path, |_, notification| {
                if let Some((parent, child)) = child_link(notification) {
                    children
                        .entry(parent.into())
                        .or_default()
                        .insert(child.into());
                }
                Ok(())
            })?;
        }
        let mut wanted = HashSet::from([id.to_owned()]);
        let mut pending = vec![id.to_owned()];
        while let Some(parent) = pending.pop() {
            for child in children.remove(&parent).into_iter().flatten() {
                if wanted.insert(child.clone()) {
                    pending.push(child);
                }
            }
        }
        for path in paths {
            let mut source = None;
            scan(&path, |offset, notification| {
                if session_id(notification).is_some_and(|id| wanted.contains(id))
                    || child_link(notification).is_some_and(|(parent, _)| wanted.contains(parent))
                {
                    let source = match source {
                        Some(source) => source,
                        None => *source.insert(self.archive.source(&path)?),
                    };
                    self.project(notification, Record { source, offset })?;
                }
                Ok(())
            })?;
        }
        for id in wanted {
            if let Some(session) = self.sessions.get_mut(&id) {
                session.flush(&mut self.archive)?;
                session.settle();
            }
        }
        Ok(())
    }

    /// Apply events still waiting on a missing predecessor at a lifecycle boundary.
    pub fn flush(&mut self) -> io::Result<()> {
        for session in self.sessions.values_mut() {
            session.flush(&mut self.archive)?;
        }
        Ok(())
    }

    /// Settle pending projections when their runtime disconnects.
    pub fn disconnect(&mut self) -> io::Result<()> {
        self.flush()?;
        for session in self.sessions.values_mut() {
            session.settle();
        }
        Ok(())
    }
}

impl Session {
    fn settle(&mut self) {
        self.running = false;
        for turn in self.activity.values_mut().filter(|turn| turn.running > 0) {
            for tool in &mut turn.tools {
                if tool.output.is_none() {
                    tool.interrupted = true;
                }
            }
            turn.interrupted += turn.running;
            turn.running = 0;
        }
    }

    fn queue(
        &mut self,
        seq: u64,
        record: Record,
        event: &Value,
        archive: &mut Archive,
    ) -> io::Result<()> {
        if self.ordered.contains_key(&seq) || self.pending.contains_key(&seq) {
            return Ok(());
        }
        if self.last_seq.is_some_and(|last| seq < last) {
            self.ordered.insert(seq, record);
            return self.rebuild(archive);
        }
        self.pending.insert(seq, record);
        while let Some((&next, _)) = self.pending.first_key_value() {
            if self
                .last_seq
                .is_some_and(|last| next > last.saturating_add(1))
                && self.pending.len() < REORDER_WINDOW
            {
                break;
            }
            self.apply_next(archive, (next == seq).then_some(event))?;
        }
        Ok(())
    }

    fn flush(&mut self, archive: &mut Archive) -> io::Result<()> {
        while !self.pending.is_empty() {
            self.apply_next(archive, None)?;
        }
        Ok(())
    }

    fn apply_next(&mut self, archive: &mut Archive, event: Option<&Value>) -> io::Result<()> {
        if let Some((seq, record)) = self.pending.pop_first() {
            self.last_seq = Some(seq);
            self.ordered.insert(seq, record);
            match event {
                Some(event) => self.project(event, archive)?,
                None => self.project(&event_at(archive, record)?, archive)?,
            };
        }
        Ok(())
    }

    fn rebuild(&mut self, archive: &mut Archive) -> io::Result<()> {
        self.rows.clear();
        self.activity.clear();
        self.group_rows.clear();
        self.calls.clear();
        self.approvals.clear();
        self.failures.clear();
        self.route = None;
        self.cache = None;
        self.title = None;
        self.input_tokens = None;
        self.output_tokens = None;
        self.context_window = None;
        self.todos.clear();
        self.plan = None;
        self.edited_paths.clear();
        self.summary = None;
        self.unhandled = 0;
        self.generation += 1;
        let ordered = std::mem::take(&mut self.ordered);
        for record in ordered.values() {
            self.project(&event_at(archive, *record)?, archive)?;
        }
        self.ordered = ordered;
        self.rows
            .extend(self.children.iter().cloned().map(Row::SubAgent));
        Ok(())
    }

    fn project(&mut self, event: &Value, archive: &mut Archive) -> io::Result<()> {
        let data = &event["data"];
        match event["type"].as_str().unwrap_or("unknown") {
            "user/message" if data["source"]["kind"].as_str() == Some("user") => {
                self.text(TextKind::UserMessage, blocks(&data["content"]), archive)?;
            }
            "user/message" => {}
            "assistant/message" => {
                let reasoning = stream_text(&data["stream"], "reasoning-chunks");
                self.text(TextKind::Reasoning, reasoning, archive)?;
                let mut body = blocks(&data["message"]["content"]);
                if body.is_empty() {
                    body = stream_text(&data["stream"], "text-chunks");
                }
                if !body.is_empty() {
                    let body = archive.text(body)?;
                    self.summary = Some(body.clone());
                    self.rows.push(Row::Text {
                        kind: TextKind::AssistantMessage,
                        body,
                    });
                }
                let usage = &data["usage"];
                self.input_tokens = usage["inputTokens"].as_u64();
                self.output_tokens = usage["outputTokens"].as_u64();
                let read = usage["cacheReadTokens"].as_u64();
                let write = usage["cacheWriteTokens"].as_u64();
                self.cache =
                    (read.is_some() || write.is_some()).then_some(CacheUsage { read, write });
                if data["interrupted"] == true {
                    self.text(
                        TextKind::Notice,
                        "Assistant output interrupted".into(),
                        archive,
                    )?;
                }
            }
            "turn/end" => {
                if let Some(kind) = data["reason"]["kind"].as_str() {
                    self.outcome = Some(kind.into());
                    if kind == "error" {
                        let message = data["reason"]["error"]["message"]
                            .as_str()
                            .unwrap_or("The turn failed");
                        self.text(TextKind::Error, message.into(), archive)?;
                    } else if let Some(message) = match kind {
                        "blocked" => Some("Turn blocked"),
                        "aborted" => Some("Turn cancelled"),
                        "interrupted" => Some("Turn interrupted"),
                        "max-tokens" => Some("Response reached its token limit"),
                        _ => None,
                    } {
                        self.text(TextKind::Notice, message.into(), archive)?;
                    }
                }
            }
            "tool/call" => self.tool(event, false, archive)?,
            "tool/result" => self.tool(event, true, archive)?,
            "model/selection" => {
                if let Some(route) = ModelRoute::from_event(data) {
                    self.context_window = None;
                    self.route = Some(route);
                }
            }
            "request/header" => {
                if let Some(route) = ModelRoute::from_event(&data["header"]["config"]) {
                    if self.route.as_ref().is_some_and(|current| {
                        current.provider != route.provider || current.model != route.model
                    }) {
                        self.context_window = None;
                    }
                    self.route = Some(route);
                }
            }
            "request/context" => self.context_window = data["contextWindow"].as_u64(),
            "session/title" => self.title = data["title"].as_str().map(metadata),
            "plan/mode" => self.plan = data["active"].as_bool(),
            "todo/write" => {
                self.todos.clear();
                for item in data["todos"].as_array().into_iter().flatten() {
                    let status = match item["status"].as_str() {
                        Some("pending") => TodoStatus::Pending,
                        Some("in_progress") => TodoStatus::InProgress,
                        Some("completed") => TodoStatus::Completed,
                        _ => continue,
                    };
                    if let Some(content) = item["content"].as_str() {
                        self.todos.push(Todo {
                            content: archive.text(content.into())?,
                            status,
                        });
                    }
                }
                let complete = self
                    .todos
                    .iter()
                    .filter(|todo| todo.status == TodoStatus::Completed)
                    .count();
                self.text(
                    TextKind::Todo,
                    format!(
                        "Checklist updated · {complete}/{} complete",
                        self.todos.len()
                    ),
                    archive,
                )?;
            }
            "approval/asked" => {
                if let (Some(key), Some(tool)) = (data["id"].as_str(), data["toolName"].as_str()) {
                    let tool = metadata(tool);
                    self.approvals
                        .insert(key.into(), (self.rows.len(), tool.clone()));
                    self.text(
                        TextKind::Approval,
                        format!("Approval requested · {tool}"),
                        archive,
                    )?;
                }
            }
            "approval/decided" => {
                if let (Some(key), Some(outcome)) = (data["id"].as_str(), data["outcome"].as_str())
                {
                    if let Some((row, tool)) = self.approvals.remove(key) {
                        let status = match outcome {
                            "allowed-once" => "Approved once",
                            "rejected" => "Approval declined",
                            "cancelled" => "Approval cancelled",
                            _ => "Approval unavailable",
                        };
                        self.rows[row] = Row::Text {
                            kind: TextKind::Approval,
                            body: archive.text(format!("{status} · {tool}"))?,
                        };
                    }
                }
            }
            "command/done" => {
                if let Some(message) = data["text"].as_str() {
                    let kind = if data["kind"] == "error" {
                        TextKind::Error
                    } else {
                        TextKind::Notice
                    };
                    self.text(kind, message.into(), archive)?;
                }
            }
            "turn/start" | "step/start" | "step/end" | "system/message" | "developer/message"
            | "session/end-seed" | "assistant/attempt" | "command/run" | "approval/policy" => {}
            _ => self.unhandled += 1,
        }
        Ok(())
    }

    fn text(&mut self, kind: TextKind, body: String, archive: &mut Archive) -> io::Result<()> {
        if !body.is_empty() {
            self.rows.push(Row::Text {
                kind,
                body: archive.text(body)?,
            });
        }
        Ok(())
    }

    fn tool(&mut self, event: &Value, result: bool, archive: &mut Archive) -> io::Result<()> {
        let data = &event["data"];
        let Some(number) = data["turn"].as_u64() else {
            return Ok(());
        };
        let message = &data["message"];
        let id = if result {
            message["toolCallId"].as_str()
        } else {
            data["callId"].as_str()
        };
        let Some(id) = id else { return Ok(()) };
        let calls = self.calls.entry(number).or_default();
        let (group, index) = if let Some(&location) = calls.get(id) {
            location
        } else {
            let group = match self.rows.last() {
                Some(Row::ToolActivity(group)) if self.activity[group].number == number => *group,
                _ => {
                    let Some(seq) = event["seq"].as_u64() else {
                        return Ok(());
                    };
                    self.group_rows.insert(seq, self.rows.len());
                    self.rows.push(Row::ToolActivity(seq));
                    self.activity.insert(
                        seq,
                        Turn {
                            number,
                            ..Turn::default()
                        },
                    );
                    seq
                }
            };
            let turn = self
                .activity
                .get_mut(&group)
                .ok_or_else(|| io::Error::other("missing activity group"))?;
            turn.tools.push(Tool {
                id: id.into(),
                ..Tool::default()
            });
            turn.running += 1;
            let index = turn.tools.len() - 1;
            calls.insert(id.into(), (group, index));
            (group, index)
        };
        let row = self.group_rows[&group];
        let turn = self
            .activity
            .get_mut(&group)
            .ok_or_else(|| io::Error::other("missing activity group"))?;
        let tool = &mut turn.tools[index];
        if result && tool.output.is_none() {
            tool.output = Some(archive.text(blocks(&message["content"]))?);
            if std::mem::take(&mut tool.interrupted) {
                turn.interrupted -= 1;
            } else {
                turn.running -= 1;
            }
            tool.failed = message["isError"] == true;
            tool.error = data["error"]["reason"]
                .as_str()
                .map(|reason| archive.text(reason.into()))
                .transpose()?;
            if tool.failed {
                turn.failed += 1;
                self.failures.entry(row).or_insert(index);
            }
        } else if !result && tool.name.is_empty() {
            tool.name = data["name"].as_str().unwrap_or("tool").into();
            tool.step = data["step"].as_u64();
            *turn.counts.entry(tool.name.clone()).or_default() += 1;
            let args = data["arguments"].as_str().unwrap_or("");
            if matches!(tool.name.as_str(), "read" | "edit" | "write") {
                tool.path = serde_json::from_str::<Value>(args)
                    .ok()
                    .and_then(|value| value["file_path"].as_str().map(metadata));
            }
            tool.arguments = archive.text(args.into())?;
        }
        if tool.output.is_some() && !tool.failed && matches!(tool.name.as_str(), "edit" | "write") {
            if let Some(path) = &tool.path {
                self.edited_paths.insert(path.clone());
            }
        }
        Ok(())
    }
}

fn metadata(text: &str) -> String {
    text.chars().take(256).collect()
}

fn event_at(archive: &mut Archive, record: Record) -> io::Result<Value> {
    if let Frame::Notification(notification) = archive.read(record)? {
        if let Some(Value::Object(mut params)) = notification.params {
            if let Some(event) = params.remove("event") {
                return Ok(event);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "archived session event is missing",
    ))
}

fn session_id(notification: &Notification) -> Option<&str> {
    let params = notification.params.as_ref()?;
    match notification.method.as_str() {
        "session.event" | "session.status" => params["sessionId"].as_str(),
        _ => None,
    }
}

fn child_link(notification: &Notification) -> Option<(&str, &str)> {
    let method = notification.method.as_str();
    if method != ServerNotification::SubagentStarted.as_str()
        && method != ServerNotification::SubagentFinished.as_str()
    {
        return None;
    }
    let params = notification.params.as_ref()?;
    Some((
        params["parentSessionId"].as_str()?,
        params["childSessionId"].as_str()?,
    ))
}

fn scan(
    path: &Path,
    mut receive: impl FnMut(u64, &Notification) -> io::Result<()>,
) -> io::Result<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    let mut offset = 0;
    loop {
        line.clear();
        use std::io::Read;
        reader
            .by_ref()
            .take(dsh_protocol::stdio::MAX_FRAME_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if line.is_empty() {
            return Ok(());
        }
        if line.len() <= dsh_protocol::stdio::MAX_FRAME_BYTES && !line.ends_with(b"\n") {
            // A process can be interrupted during the append, leaving an incomplete final record.
            return Ok(());
        }
        if line.len() > dsh_protocol::stdio::MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archived frame exceeds size limit",
            ));
        }
        if let Frame::Notification(notification) =
            serde_json::from_slice(&line).map_err(io::Error::other)?
        {
            receive(offset, &notification)?;
        }
        offset += line.len() as u64;
    }
}

fn stream_text(stream: &Value, kind: &str) -> String {
    stream
        .as_array()
        .into_iter()
        .flatten()
        .filter(|chunk| chunk["type"] == kind)
        .flat_map(|chunk| chunk["texts"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect()
}

/// Render ordinary text blocks and keep non-text blocks legible as placeholders.
pub fn blocks(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    let mut text = String::new();
    for block in value.as_array().into_iter().flatten() {
        let kind = block["type"].as_str().unwrap_or("content");
        match kind {
            "text" => text.push_str(block["text"].as_str().unwrap_or_default()),
            "reasoning" | "tool-call" => {}
            kind => {
                text.push('[');
                text.push_str(kind);
                text.push(']');
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!(
                "dsh-state-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            for entry in fs::read_dir(&self.0).into_iter().flatten().flatten() {
                let _ = fs::remove_file(entry.path());
            }
            let _ = fs::remove_dir(&self.0);
        }
    }

    fn notify(method: ServerNotification, params: Value) -> Notification {
        Notification {
            jsonrpc: "2.0".into(),
            method: method.as_str().into(),
            params: Some(params),
        }
    }

    fn event(seq: u64, kind: &str, data: Value) -> Notification {
        notify(
            ServerNotification::SessionEvent,
            json!({"sessionId":"root", "event":{"type":kind,"seq":seq,"time":1,"data":data}}),
        )
    }

    #[test]
    fn projects_semantic_notices_without_leaking_raw_events() {
        let mut chat = Conversation::default();
        let unseen = event(7, "future/internal", json!({"opaque":"raw-only"}));
        for message in [
            event(1, "session/title", json!({"title":"Review workspace"})),
            event(2, "request/context", json!({"contextWindow":128000})),
            event(
                3,
                "todo/write",
                json!({"todos":[{"content":"Review diff","status":"completed"}]}),
            ),
            event(
                4,
                "approval/asked",
                json!({"id":"request-1","toolName":"bash","reason":"raw reason"}),
            ),
            event(
                5,
                "approval/decided",
                json!({"id":"request-1","outcome":"rejected"}),
            ),
            event(
                6,
                "command/done",
                json!({"kind":"error","text":"No command found"}),
            ),
            unseen.clone(),
        ] {
            chat.push(message).unwrap();
        }
        let session = &chat.sessions["root"];
        assert_eq!(session.title.as_deref(), Some("Review workspace"));
        assert_eq!(session.context_window, Some(128000));
        let rows: Vec<_> = session
            .rows
            .iter()
            .map(|row| match row {
                Row::Text { kind, body } => (*kind, chat.body(body).unwrap().to_string()),
                _ => panic!("expected text row"),
            })
            .collect();
        assert_eq!(
            rows,
            [
                (TextKind::Todo, "Checklist updated · 1/1 complete".into()),
                (TextKind::Approval, "Approval declined · bash".into()),
                (TextKind::Error, "No command found".into()),
            ]
        );
        let (seq, raw) = chat.debug_event("root", None, false).unwrap().unwrap();
        assert_eq!(seq, 7);
        assert_eq!(raw, unseen.params.unwrap()["event"]);
        assert_eq!(
            chat.debug_event("root", Some(seq), false)
                .unwrap()
                .unwrap()
                .0,
            6
        );
        assert!(chat.debug_event("root", Some(seq), true).unwrap().is_none());
    }

    #[test]
    fn groups_consecutive_calls_and_matches_results_after_a_message_boundary() {
        let mut chat = Conversation::default();
        for message in [
            event(
                1,
                "tool/call",
                json!({"turn":1,"callId":"read-1","name":"read"}),
            ),
            event(
                2,
                "tool/call",
                json!({"turn":1,"callId":"edit-1","name":"edit","arguments":"{\"file_path\":\"src/main.rs\"}"}),
            ),
            event(
                3,
                "assistant/message",
                json!({"message":{"content":"Checking another file"}}),
            ),
            event(
                4,
                "tool/call",
                json!({"turn":1,"callId":"read-2","name":"read"}),
            ),
            event(
                5,
                "tool/result",
                json!({"turn":1,"message":{"toolCallId":"edit-1","content":"changed"}}),
            ),
            event(
                6,
                "tool/result",
                json!({"turn":1,"message":{"toolCallId":"read-2","content":"missing","isError":true}}),
            ),
        ] {
            chat.push(message).unwrap();
        }
        let session = &chat.sessions["root"];
        assert!(matches!(
            session.rows.as_slice(),
            [
                Row::ToolActivity(1),
                Row::Text {
                    kind: TextKind::AssistantMessage,
                    ..
                },
                Row::ToolActivity(4)
            ]
        ));
        assert_eq!(
            session.activity[&1].counts,
            [("edit".into(), 1), ("read".into(), 1)].into()
        );
        assert_eq!(session.activity[&1].running, 1);
        assert_eq!(session.activity[&4].failed, 1);
        assert_eq!(session.failures.get(&2), Some(&0));
        assert!(session.edited_paths.contains("src/main.rs"));
    }

    #[test]
    fn latest_route_and_cache_distinguish_resolved_effort_zero_and_missing() {
        let mut chat = Conversation::default();
        for item in [
            event(1, "model/selection", json!({"provider":"a","model":"b"})),
            event(
                3,
                "assistant/message",
                json!({"usage":{"cacheReadTokens":0}}),
            ),
        ] {
            chat.push(item).unwrap();
        }
        chat.flush().unwrap();
        chat.push(event(
            2,
            "request/header",
            json!({"header":{"config":{"provider":"a","model":"b","reasoningEffort":"high"}}}),
        ))
        .unwrap();
        let session = &chat.sessions["root"];
        assert_eq!(
            session.route.as_ref().unwrap().reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(
            session.cache,
            Some(CacheUsage {
                read: Some(0),
                write: None
            })
        );

        chat.push(event(
            4,
            "assistant/message",
            json!({"usage":{"outputTokens":17}}),
        ))
        .unwrap();
        assert_eq!(chat.sessions["root"].cache, None);
    }

    #[test]
    fn replays_gaps_and_late_events_without_reapplying_duplicates() {
        let mut chat = Conversation::default();
        let user = event(
            1,
            "user/message",
            json!({"source":{"kind":"user"},"content":[{"type":"text","text":"你好"}]}),
        );
        let tool = event(
            2,
            "tool/call",
            json!({"turn":7,"step":3,"callId":"r","name":"read","arguments":"{}"}),
        );
        let result = event(
            3,
            "tool/result",
            json!({"turn":7,"step":3,"message":{"toolCallId":"r","content":[{"type":"text","text":"missing"}],"isError":true},"error":{"reason":"not found"}}),
        );
        let assistant = event(
            4,
            "assistant/message",
            json!({"turn":7,"step":4,"stream":[{"type":"text-chunks","texts":["A","B"]},{"type":"reasoning-chunks","texts":["C","D"]}]}),
        );
        let unknown = event(5, "future/kind", json!({"raw":[0,1]}));
        for item in [
            user.clone(),
            result.clone(),
            result.clone(),
            assistant.clone(),
            unknown.clone(),
        ] {
            chat.push(item).unwrap();
        }
        assert_eq!(chat.sessions["root"].rows.len(), 1);
        chat.flush().unwrap();
        chat.push(tool.clone()).unwrap();
        let session = &chat.sessions["root"];
        assert_eq!(session.generation, 1);
        assert_eq!(session.activity[&2].tools[0].step, Some(3));
        assert_eq!(session.activity[&2].failed, 1);
        assert_eq!(session.failures.get(&1), Some(&0));
        assert_eq!(
            chat.body(session.activity[&2].tools[0].output.as_ref().unwrap())
                .unwrap()
                .as_str(),
            "missing"
        );
        assert_eq!(
            chat.body(session.summary.as_ref().unwrap())
                .unwrap()
                .as_str(),
            "AB"
        );
        assert!(
            matches!(&session.rows[2], Row::Text { kind: TextKind::Reasoning, body } if chat.body(body).unwrap().as_str() == "CD")
        );
        assert_eq!(session.unhandled, 1);
        assert_eq!(chat.received_count(), 6);
        assert!(
            matches!(chat.archive.read(Record { source: 0, offset: 4 }).unwrap(), Frame::Notification(item) if item.params == unknown.params)
        );
        let mut ordered = Conversation::default();
        for item in [user, tool, result, assistant, unknown] {
            ordered.push(item).unwrap();
        }
        assert_eq!(session.rows, ordered.sessions["root"].rows);
        assert_eq!(session.activity, ordered.sessions["root"].activity);
    }

    #[test]
    fn links_children_once_and_marks_unfinished_tools_on_disconnect() {
        let mut chat = Conversation::default();
        let started = notify(
            ServerNotification::SubagentStarted,
            json!({"parentSessionId":"root","childSessionId":"child","agentId":"a"}),
        );
        chat.push(started.clone()).unwrap();
        chat.push(started).unwrap();
        chat.push(event(
            8,
            "tool/call",
            json!({"turn":1,"callId":"r","name":"read"}),
        ))
        .unwrap();
        assert!(chat.sessions["child"].running);
        assert_eq!(chat.sessions["child"].parent.as_deref(), Some("root"));
        chat.push(notify(
            ServerNotification::SubagentFinished,
            json!({"parentSessionId":"root","childSessionId":"child","status":"ok"}),
        ))
        .unwrap();
        chat.disconnect().unwrap();
        let parent = &chat.sessions["root"];
        assert_eq!(
            parent.rows,
            [Row::SubAgent("child".into()), Row::ToolActivity(8)]
        );
        assert_eq!(parent.activity[&8].running, 0);
        assert!(parent.activity[&8].tools[0].interrupted);
        assert_eq!(chat.sessions["child"].outcome.as_deref(), Some("ok"));
    }

    #[test]
    fn disk_journal_retains_original_frames_and_reopens_after_a_torn_tail() {
        let work = Scratch::new();
        let path = work.0.join("01.ndjson");
        let request = Request {
            jsonrpc: "2.0".into(),
            id: dsh_protocol::protocol::JsonRpcId::String("req_approval".into()),
            method: "approval/request".into(),
            params: Some(json!({"opaque":[1,2,3]})),
        };
        let unknown = event(3, "future/kind", json!({"raw":[0,1]}));
        let mut child = event(
            1,
            "assistant/message",
            json!({"message":{"content":"child body"}}),
        );
        child.params.as_mut().unwrap()["sessionId"] = json!("child");
        {
            let mut chat = Conversation::open(&work.0, "01").unwrap();
            chat.push(notify(
                ServerNotification::SubagentStarted,
                json!({"parentSessionId":"root","childSessionId":"child"}),
            ))
            .unwrap();
            chat.push(child).unwrap();
            for item in [
                unknown.clone(),
                unknown.clone(),
                event(
                    2,
                    "assistant/message",
                    json!({"message":{"content":"older"}}),
                ),
            ] {
                chat.push(item).unwrap();
            }
            chat.request(request.clone()).unwrap();
            chat.push(notify(
                ServerNotification::SubagentFinished,
                json!({"parentSessionId":"root","childSessionId":"child","status":"ok"}),
            ))
            .unwrap();
            chat.sync().unwrap();
            assert_eq!(chat.received_count(), 7);
            assert_eq!(
                chat.history().unwrap().sessions().unwrap(),
                [("root".into(), None)]
            );
        }
        assert!(!work.0.join("01.spool").exists());
        let mut reader = BufReader::new(File::open(&path).unwrap());
        let records: Vec<_> =
            std::iter::from_fn(|| dsh_protocol::stdio::read_frame(&mut reader).unwrap()).collect();
        assert_eq!(records.len(), 7);
        assert_eq!(records[2], Frame::Notification(unknown.clone()));
        assert_eq!(records[3], Frame::Notification(unknown));
        assert_eq!(records[5], Frame::Request(request));
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"jsonrpc\":")
            .unwrap();

        let mut chat = Conversation::open(&work.0, "02").unwrap();
        assert_eq!(
            chat.history().unwrap().sessions().unwrap(),
            [("root".into(), None)]
        );
        chat.replay("root").unwrap();
        chat.replay("root").unwrap();
        let root = &chat.sessions["root"];
        assert_eq!(root.unhandled, 1);
        assert_eq!(root.rows.len(), 2);
        assert_eq!(
            chat.body(root.summary.as_ref().unwrap()).unwrap().as_str(),
            "older"
        );
        let child = &chat.sessions["child"];
        assert_eq!(child.outcome.as_deref(), Some("ok"));
        assert_eq!(
            chat.body(child.summary.as_ref().unwrap()).unwrap().as_str(),
            "child body"
        );
        assert_eq!(chat.received_count(), 0);
    }

    #[test]
    fn offline_replay_preserves_source_and_reloads_evicted_output() {
        let work = Scratch::new();
        let source = work.0.join("source.ndjson");
        {
            let mut chat = Conversation::open(&work.0, "source").unwrap();
            for (seq, ch) in [(1, 'a'), (2, 'b'), (3, 'c')] {
                chat.push(event(
                    seq,
                    "assistant/message",
                    json!({"message":{"content":ch.to_string().repeat(3 * 1024 * 1024)}}),
                ))
                .unwrap();
            }
        }
        let original = fs::read(&source).unwrap();
        let (chat, id, events) = Conversation::open_replay(&work.0, "offline", &source).unwrap();
        assert_eq!((id.as_str(), events), ("root", 3));
        let Row::Text { body: first, .. } = &chat.sessions[&id].rows[0] else {
            panic!("missing transcript body")
        };
        let original_body = Arc::downgrade(&chat.body(first).unwrap());
        assert!(original_body.upgrade().is_some());
        for row in &chat.sessions[&id].rows[1..] {
            let Row::Text { body, .. } = row else {
                panic!("missing transcript body")
            };
            assert_eq!(chat.body(body).unwrap().len(), 3 * 1024 * 1024);
        }
        assert!(original_body.upgrade().is_none());
        assert_eq!(
            chat.body(first).unwrap().as_str(),
            "a".repeat(3 * 1024 * 1024)
        );
        assert!(!work.0.join("offline.ndjson").exists());
        drop(chat);
        assert!(!work.0.join("offline.spool").exists());
        assert_eq!(fs::read(source).unwrap(), original);
    }
}
