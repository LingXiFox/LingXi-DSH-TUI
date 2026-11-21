//! Ordered SDK event journal and turn-based transcript projection.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use dsh_protocol::protocol::{Notification, Request, ServerNotification};
use serde_json::Value;

const REORDER_WINDOW: usize = 32;

/// One original inbound SDK message, in arrival order.
#[derive(Debug, Clone)]
pub enum Received {
    /// A durable session event or other SDK notification.
    Notification(Notification),
    /// An SDK-initiated interaction, including unsupported methods.
    Request(Request),
}

/// Display rows identified by their index inside a session.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A human message, assistant message, or status notice.
    Text {
        /// Speaker or notice kind.
        label: &'static str,
        /// Terminal-independent text or Markdown.
        body: Arc<str>,
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
    pub arguments: Arc<str>,
    /// Text projection of the tool's result blocks.
    pub output: Option<Arc<str>>,
    /// SDK-reported failure.
    pub failed: bool,
    /// SDK-reported user-friendly error, when present.
    pub error: Option<Arc<str>>,
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
    /// Latest token accounting, as reported by the adapter.
    pub usage: Option<Value>,
    /// Latest child completion or turn outcome.
    pub outcome: Option<String>,
    /// Latest assembled assistant text for child-session previews.
    pub summary: Option<Arc<str>>,
    /// Row indexes with failed tool calls, for direct navigation.
    pub failures: BTreeMap<usize, usize>,
    /// Number of events whose meaning is not projected by this client.
    pub unhandled: usize,
    /// Increments when rows are reconstructed after a late event.
    pub generation: u64,
    seen: HashSet<u64>,
    ordered: BTreeMap<u64, usize>,
    pending: BTreeMap<u64, usize>,
    last_seq: Option<u64>,
    group_rows: HashMap<u64, usize>,
    children: Vec<String>,
}

/// Raw SDK messages and their derived session views.
#[derive(Debug, Default)]
pub struct Conversation {
    /// Every received message, including duplicates and unknown methods.
    pub received: Vec<Received>,
    /// Projections keyed by SDK session ID.
    pub sessions: HashMap<String, Session>,
}

impl Conversation {
    /// Record a host request without conflating it with a session-log event.
    pub fn request(&mut self, request: Request) {
        self.received.push(Received::Request(request));
    }

    /// Journal first, then project recognized notifications.
    pub fn push(&mut self, notification: Notification) {
        let index = self.received.len();
        self.received.push(Received::Notification(notification));
        let Received::Notification(notification) = &self.received[index] else {
            return;
        };
        let Some(params) = &notification.params else {
            return;
        };
        let method = notification.method.as_str();
        if method == ServerNotification::SessionEvent.as_str() {
            let (Some(id), Some(seq)) = (
                params["sessionId"].as_str(),
                params["event"]["seq"].as_u64(),
            ) else {
                return;
            };
            self.sessions
                .entry(id.into())
                .or_default()
                .queue(seq, index, &self.received);
        } else if method == ServerNotification::SessionStatus.as_str() {
            let Some(id) = params["sessionId"].as_str() else {
                return;
            };
            let session = self.sessions.entry(id.into()).or_default();
            match params["status"].as_str() {
                Some("idle") => {
                    session.running = false;
                    session.flush(&self.received);
                }
                Some("running") => session.running = true,
                _ => {}
            }
        } else if method == ServerNotification::SubagentStarted.as_str()
            || method == ServerNotification::SubagentFinished.as_str()
        {
            let (Some(parent), Some(child)) = (
                params["parentSessionId"].as_str(),
                params["childSessionId"].as_str(),
            ) else {
                return;
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
                session.flush(&self.received);
            } else {
                session.running = true;
            }
        }
    }

    /// Apply events still waiting on a missing predecessor at a lifecycle boundary.
    pub fn flush(&mut self) {
        for session in self.sessions.values_mut() {
            session.flush(&self.received);
        }
    }

    /// Settle pending projections when their runtime disconnects.
    pub fn disconnect(&mut self) {
        self.flush();
        for session in self.sessions.values_mut() {
            session.running = false;
            for turn in session.turns.values_mut().filter(|turn| turn.running > 0) {
                for tool in &mut turn.tools {
                    if tool.output.is_none() {
                        tool.interrupted = true;
                    }
                }
                turn.running = 0;
            }
        }
    }
}

impl Session {
    fn queue(&mut self, seq: u64, index: usize, journal: &[Received]) {
        if !self.seen.insert(seq) {
            return;
        }
        if self.last_seq.is_some_and(|last| seq < last) {
            self.ordered.insert(seq, index);
            self.rebuild(journal);
            return;
        }
        self.pending.insert(seq, index);
        while let Some((&next, _)) = self.pending.first_key_value() {
            if self
                .last_seq
                .is_some_and(|last| next > last.saturating_add(1))
                && self.pending.len() < REORDER_WINDOW
            {
                break;
            }
            self.apply_next(journal);
        }
    }

    fn flush(&mut self, journal: &[Received]) {
        while !self.pending.is_empty() {
            self.apply_next(journal);
        }
    }

    fn apply_next(&mut self, journal: &[Received]) {
        if let Some((seq, index)) = self.pending.pop_first() {
            self.last_seq = Some(seq);
            self.ordered.insert(seq, index);
            if let Some(event) = event_at(journal, index) {
                self.project(event);
            }
        }
    }

    fn rebuild(&mut self, journal: &[Received]) {
        self.rows.clear();
        self.turns.clear();
        self.group_rows.clear();
        self.failures.clear();
        self.usage = None;
        self.summary = None;
        self.unhandled = 0;
        self.generation += 1;
        let ordered = std::mem::take(&mut self.ordered);
        for index in ordered.values() {
            if let Some(event) = event_at(journal, *index) {
                self.project(event);
            }
        }
        self.ordered = ordered;
        self.rows
            .extend(self.children.iter().cloned().map(Row::Child));
    }

    fn project(&mut self, event: &Value) {
        let data = &event["data"];
        match event["type"].as_str().unwrap_or("unknown") {
            "user/message" if data["source"]["kind"].as_str() == Some("user") => {
                self.text("You", blocks(&data["content"]));
            }
            "user/message" => {}
            "assistant/message" => {
                let mut body = blocks(&data["message"]["content"]);
                if body.is_empty() {
                    body = stream_text(&data["stream"], "text-chunks");
                }
                if !body.is_empty() {
                    let body: Arc<str> = body.into();
                    self.summary = Some(body.clone());
                    self.rows.push(Row::Text {
                        label: "Assistant",
                        body,
                    });
                }
                let reasoning = stream_text(&data["stream"], "reasoning-chunks");
                self.text("Reasoning", reasoning);
                if !data["usage"].is_null() {
                    self.usage = Some(data["usage"].clone());
                }
                if data["interrupted"] == true {
                    self.text("Notice", "Assistant output interrupted".into());
                }
            }
            "turn/end" => {
                if let Some(kind) = data["reason"]["kind"].as_str() {
                    self.outcome = Some(kind.into());
                    if !data["reason"]["error"].is_null() {
                        self.text("Error", data["reason"]["error"].to_string());
                    }
                }
            }
            "tool/call" => self.tool(data, false),
            "tool/result" => self.tool(data, true),
            "turn/start" | "step/start" | "step/end" | "request/header" | "request/context"
            | "system/message" | "developer/message" | "session/end-seed" | "assistant/attempt" => {
            }
            kind => {
                self.unhandled += 1;
                if kind.starts_with("command/")
                    || kind.starts_with("todo/")
                    || kind.starts_with("plan/")
                    || kind.starts_with("approval/")
                {
                    self.text("Event", format!("{kind} · {data}"));
                }
            }
        }
    }

    fn text(&mut self, label: &'static str, body: String) {
        if !body.is_empty() {
            self.rows.push(Row::Text {
                label,
                body: body.into(),
            });
        }
    }

    fn tool(&mut self, data: &Value, result: bool) {
        let Some(number) = data["turn"].as_u64() else {
            return;
        };
        let message = &data["message"];
        let id = if result {
            message["toolCallId"].as_str()
        } else {
            data["callId"].as_str()
        };
        let Some(id) = id else { return };
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
            tool.output = Some(blocks(&message["content"]).into());
            if !std::mem::take(&mut tool.interrupted) {
                turn.running -= 1;
            }
            tool.failed = message["isError"] == true;
            tool.error = data["error"]["reason"].as_str().map(Arc::from);
            if tool.failed {
                turn.failed += 1;
                self.failures.entry(row).or_insert(index);
            }
        } else if !result && tool.name.is_empty() {
            tool.name = data["name"].as_str().unwrap_or("tool").into();
            tool.step = data["step"].as_u64();
            *turn.counts.entry(tool.name.clone()).or_default() += 1;
            tool.arguments = data["arguments"].as_str().unwrap_or("").into();
        }
    }
}

fn event_at(journal: &[Received], index: usize) -> Option<&Value> {
    let Received::Notification(notification) = journal.get(index)? else {
        return None;
    };
    notification.params.as_ref()?.get("event")
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
            chat.push(item);
        }
        assert_eq!(chat.sessions["root"].rows.len(), 1);
        chat.flush();
        chat.push(tool.clone());
        let session = &chat.sessions["root"];
        assert_eq!(session.generation, 1);
        assert_eq!(session.turns[&7].tools[0].step, Some(3));
        assert_eq!(session.turns[&7].failed, 1);
        assert_eq!(session.failures.get(&1), Some(&0));
        assert_eq!(
            session.turns[&7].tools[0].output.as_deref(),
            Some("missing")
        );
        assert_eq!(session.summary.as_deref(), Some("AB"));
        assert!(
            matches!(&session.rows[3], Row::Text { label: "Reasoning", body } if &**body == "CD")
        );
        assert_eq!(session.unhandled, 1);
        assert_eq!(chat.received.len(), 6);
        assert!(
            matches!(&chat.received[4], Received::Notification(item) if item.params == unknown.params)
        );
        let mut ordered = Conversation::default();
        for item in [user, tool, result, assistant, unknown] {
            ordered.push(item);
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
        chat.push(started.clone());
        chat.push(started);
        chat.push(event(
            8,
            "tool/call",
            json!({"turn":1,"callId":"r","name":"read"}),
        ));
        assert!(chat.sessions["child"].running);
        assert_eq!(chat.sessions["child"].parent.as_deref(), Some("root"));
        chat.push(notify(
            ServerNotification::SubagentFinished,
            json!({"parentSessionId":"root","childSessionId":"child","status":"ok"}),
        ));
        chat.disconnect();
        let parent = &chat.sessions["root"];
        assert_eq!(parent.rows, [Row::Child("child".into()), Row::Tools(1)]);
        assert_eq!(parent.turns[&1].running, 0);
        assert!(parent.turns[&1].tools[0].interrupted);
        assert_eq!(chat.sessions["child"].outcome.as_deref(), Some("ok"));
    }
}
