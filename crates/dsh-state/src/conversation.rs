//! Ordered SDK event journal and turn-based transcript projection.

use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// A human message, assistant message, or status notice.
    Text {
        /// Speaker or notice kind.
        label: &'static str,
        /// Terminal-independent text or Markdown.
        body: Text,
    },
    /// One collapsible group tied to an upstream turn number.
    Tools(u64),
    /// In-process child session linked by the official notification.
    Child(String),
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
    /// Text projection of the tool's result blocks.
    pub output: Option<Text>,
    /// SDK-reported failure.
    pub failed: bool,
    /// SDK-reported user-friendly error, when present.
    pub error: Option<Text>,
    /// The runtime disconnected before a result was observed.
    pub interrupted: bool,
}

/// Projection for one real turn number.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Turn {
    /// Calls ordered by their first recorded event.
    pub tools: Vec<Tool>,
    /// Tool name counts for the compact view.
    pub counts: BTreeMap<String, usize>,
    /// Running calls in the turn.
    pub running: usize,
    /// Failed calls in the turn.
    pub failed: usize,
    lookup: HashMap<String, usize>,
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
    /// Tool groups keyed by official turn number.
    pub turns: BTreeMap<u64, Turn>,
    /// Whole-agent status from the SDK.
    pub running: bool,
    /// Parent session if this is a local child.
    pub parent: Option<String>,
    /// Latest recorded model route.
    pub route: Option<ModelRoute>,
    /// Cache counters, if reported for the latest assistant response.
    pub cache: Option<CacheUsage>,
    /// Latest child completion or turn outcome.
    pub outcome: Option<String>,
    /// Latest assembled assistant text for child-session previews.
    pub summary: Option<Text>,
    /// Row indexes with failed tool calls, for direct navigation.
    pub failures: BTreeMap<usize, usize>,
    /// Number of events whose meaning is not projected by this client.
    pub unhandled: usize,
    /// Increments when rows are reconstructed after a late event.
    pub generation: u64,
    ordered: BTreeMap<u64, Record>,
    pending: BTreeMap<u64, Record>,
    last_seq: Option<u64>,
    group_rows: HashMap<u64, usize>,
    children: Vec<String>,
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
    /// Enumerate root sessions recorded by this Host, newest first.
    pub fn sessions(self) -> io::Result<Vec<String>> {
        let mut sessions = HashMap::new();
        let mut children = HashSet::new();
        for (run, path) in self.0.iter().enumerate() {
            scan(path, |offset, notification| {
                if let Some(id) = session_id(notification) {
                    sessions.insert(id.to_owned(), (run, offset));
                }
                if let Some((_, child)) = child_link(notification) {
                    children.insert(child.to_owned());
                }
                Ok(())
            })?;
        }
        let mut sessions: Vec<_> = sessions.into_iter().collect();
        sessions.retain(|(id, _)| !children.contains(id));
        sessions.sort_unstable_by_key(|(_, position)| std::cmp::Reverse(*position));
        Ok(sessions.into_iter().map(|(id, _)| id).collect())
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
                parent_session.rows.push(Row::Child(child.into()));
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
        for turn in self.turns.values_mut().filter(|turn| turn.running > 0) {
            for tool in &mut turn.tools {
                if tool.output.is_none() {
                    tool.interrupted = true;
                }
            }
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
        self.turns.clear();
        self.group_rows.clear();
        self.failures.clear();
        self.route = None;
        self.cache = None;
        self.summary = None;
        self.unhandled = 0;
        self.generation += 1;
        let ordered = std::mem::take(&mut self.ordered);
        for record in ordered.values() {
            self.project(&event_at(archive, *record)?, archive)?;
        }
        self.ordered = ordered;
        self.rows
            .extend(self.children.iter().cloned().map(Row::Child));
        Ok(())
    }

    fn project(&mut self, event: &Value, archive: &mut Archive) -> io::Result<()> {
        let data = &event["data"];
        match event["type"].as_str().unwrap_or("unknown") {
            "user/message" if data["source"]["kind"].as_str() == Some("user") => {
                self.text("You", blocks(&data["content"]), archive)?;
            }
            "user/message" => {}
            "assistant/message" => {
                let mut body = blocks(&data["message"]["content"]);
                if body.is_empty() {
                    body = stream_text(&data["stream"], "text-chunks");
                }
                if !body.is_empty() {
                    let body = archive.text(body)?;
                    self.summary = Some(body.clone());
                    self.rows.push(Row::Text {
                        label: "Assistant",
                        body,
                    });
                }
                let reasoning = stream_text(&data["stream"], "reasoning-chunks");
                self.text("Reasoning", reasoning, archive)?;
                let usage = &data["usage"];
                let read = usage["cacheReadTokens"].as_u64();
                let write = usage["cacheWriteTokens"].as_u64();
                self.cache =
                    (read.is_some() || write.is_some()).then_some(CacheUsage { read, write });
                if data["interrupted"] == true {
                    self.text("Notice", "Assistant output interrupted".into(), archive)?;
                }
            }
            "turn/end" => {
                if let Some(kind) = data["reason"]["kind"].as_str() {
                    self.outcome = Some(kind.into());
                    if !data["reason"]["error"].is_null() {
                        self.text("Error", data["reason"]["error"].to_string(), archive)?;
                    }
                }
            }
            "tool/call" => self.tool(data, false, archive)?,
            "tool/result" => self.tool(data, true, archive)?,
            "model/selection" => {
                if let Some(route) = ModelRoute::from_event(data) {
                    self.route = Some(route);
                }
            }
            "request/header" => {
                if let Some(route) = ModelRoute::from_event(&data["header"]["config"]) {
                    self.route = Some(route);
                }
            }
            "turn/start" | "step/start" | "step/end" | "request/context" | "system/message"
            | "developer/message" | "session/end-seed" | "assistant/attempt" => {}
            kind => {
                self.unhandled += 1;
                if kind.starts_with("command/")
                    || kind.starts_with("todo/")
                    || kind.starts_with("plan/")
                    || kind.starts_with("approval/")
                {
                    self.text("Event", format!("{kind} · {data}"), archive)?;
                }
            }
        }
        Ok(())
    }

    fn text(&mut self, label: &'static str, body: String, archive: &mut Archive) -> io::Result<()> {
        if !body.is_empty() {
            self.rows.push(Row::Text {
                label,
                body: archive.text(body)?,
            });
        }
        Ok(())
    }

    fn tool(&mut self, data: &Value, result: bool, archive: &mut Archive) -> io::Result<()> {
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
        let row = *self.group_rows.entry(number).or_insert_with(|| {
            self.rows.push(Row::Tools(number));
            self.rows.len() - 1
        });
        let turn = self.turns.entry(number).or_default();
        let index = *turn.lookup.entry(id.into()).or_insert_with(|| {
            turn.tools.push(Tool {
                id: id.into(),
                ..Tool::default()
            });
            turn.running += 1;
            turn.tools.len() - 1
        });
        let tool = &mut turn.tools[index];
        if result && tool.output.is_none() {
            tool.output = Some(archive.text(blocks(&message["content"]))?);
            if !std::mem::take(&mut tool.interrupted) {
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
            tool.arguments = archive.text(data["arguments"].as_str().unwrap_or("").into())?;
        }
        Ok(())
    }
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
        assert_eq!(session.turns[&7].tools[0].step, Some(3));
        assert_eq!(session.turns[&7].failed, 1);
        assert_eq!(session.failures.get(&1), Some(&0));
        assert_eq!(
            chat.body(session.turns[&7].tools[0].output.as_ref().unwrap())
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
            matches!(&session.rows[3], Row::Text { label: "Reasoning", body } if chat.body(body).unwrap().as_str() == "CD")
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
        assert_eq!(session.turns, ordered.sessions["root"].turns);
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
        assert_eq!(parent.rows, [Row::Child("child".into()), Row::Tools(1)]);
        assert_eq!(parent.turns[&1].running, 0);
        assert!(parent.turns[&1].tools[0].interrupted);
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
            assert_eq!(chat.history().unwrap().sessions().unwrap(), ["root"]);
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
        assert_eq!(chat.history().unwrap().sessions().unwrap(), ["root"]);
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
