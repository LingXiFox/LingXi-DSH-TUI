use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dsh_protocol::protocol::{JsonRpcId, METHOD_APPROVAL_REQUEST, METHOD_USER_QUESTION, Request};
use dsh_state::draft::Draft;
use serde_json::{Value, json};

use super::{Modal, edit_key, insert};

pub(super) struct Interaction {
    id: JsonRpcId,
    expires: Instant,
    kind: Kind,
}

enum Kind {
    Approval {
        summary: String,
        allow: bool,
    },
    Questions {
        request_id: String,
        items: Vec<Question>,
        index: usize,
        answers: Vec<Value>,
    },
}

struct Question {
    id: String,
    text: String,
    options: Vec<(String, String)>,
    multi: bool,
    selected: Vec<bool>,
    focus: usize,
    custom: Draft,
}

impl Interaction {
    pub(super) fn from_request(request: &Request, expires: Instant) -> Option<Self> {
        let params = request.params.as_ref()?;
        let kind = match request.method.as_str() {
            METHOD_APPROVAL_REQUEST => Kind::Approval {
                summary: format!(
                    "{} · {}\n{}\n{}",
                    params["toolName"].as_str()?,
                    params["sessionId"].as_str()?,
                    params["callId"].as_str().unwrap_or(""),
                    params["reason"].as_str().unwrap_or("")
                ),
                allow: false,
            },
            METHOD_USER_QUESTION => {
                let items: Vec<_> = params["questions"]
                    .as_array()?
                    .iter()
                    .map(Question::parse)
                    .collect::<Option<_>>()?;
                if items.is_empty() {
                    return None;
                }
                Kind::Questions {
                    request_id: params["requestId"].as_str()?.into(),
                    items,
                    index: 0,
                    answers: Vec::new(),
                }
            }
            _ => return None,
        };
        Some(Self {
            id: request.id.clone(),
            expires,
            kind,
        })
    }

    pub(super) fn id(&self) -> &JsonRpcId {
        &self.id
    }

    pub(super) fn request_id(&self) -> Option<&str> {
        match &self.kind {
            Kind::Questions { request_id, .. } => Some(request_id),
            Kind::Approval { .. } => None,
        }
    }

    pub(super) fn expires(&self) -> Instant {
        self.expires
    }

    pub(super) fn modal(&self) -> Modal<'_> {
        match &self.kind {
            Kind::Approval { summary, allow } => {
                let mut modal = Modal::text(" Tool approval ", summary.as_str());
                modal.warning = true;
                modal.hint = " ←/→ choose · PgUp/PgDn details · Enter confirm · Esc cancel ";
                modal.list = Some((
                    vec!["Deny".into(), "Allow once".into()],
                    usize::from(*allow),
                ));
                modal
            }
            Kind::Questions { items, index, .. } if *index < items.len() => {
                items[*index].modal(*index, items.len())
            }
            Kind::Questions { .. } => Modal::text(" Agent question ", "Answers ready"),
        }
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> Option<Value> {
        let cancel = key.code == KeyCode::Esc
            || key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.kind {
            Kind::Approval { allow, .. } => {
                if cancel {
                    return Some(json!({"outcome":"cancelled"}));
                }
                match key.code {
                    KeyCode::Left | KeyCode::Up => *allow = false,
                    KeyCode::Right | KeyCode::Down => *allow = true,
                    KeyCode::Tab => *allow = !*allow,
                    KeyCode::Enter => {
                        return Some(
                            json!({"outcome": if *allow { "allowed-once" } else { "rejected" }}),
                        );
                    }
                    _ => {}
                }
                None
            }
            Kind::Questions {
                items,
                index,
                answers,
                ..
            } => {
                if cancel {
                    return Some(json!({"cancelled":true}));
                }
                if *index == items.len() {
                    return Some(json!({"answers":answers}));
                }
                if let Some(answer) = items[*index].key(key) {
                    answers.push(answer);
                    *index += 1;
                    if *index == items.len() {
                        return Some(json!({"answers":answers}));
                    }
                }
                None
            }
        }
    }

    pub(super) fn paste(&mut self, text: &str) {
        let item = match &mut self.kind {
            Kind::Questions { items, index, .. } => items.get_mut(*index),
            _ => None,
        };
        if let Some(item) = item {
            item.focus = item.options.len();
            insert(&mut item.custom, text);
        }
    }
}

impl Question {
    fn parse(value: &Value) -> Option<Self> {
        let options: Vec<_> = value["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|option| {
                Some((
                    option["label"].as_str()?.into(),
                    option["description"].as_str().unwrap_or("").into(),
                ))
            })
            .collect();
        Some(Self {
            id: value["id"].as_str()?.into(),
            text: [
                value["header"].as_str(),
                Some(value["question"].as_str()?),
                value["detail"].as_str(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
            multi: value["multiSelect"].as_bool().unwrap_or(false),
            selected: vec![false; options.len()],
            options,
            focus: 0,
            custom: Draft::default(),
        })
    }

    fn modal(&self, index: usize, count: usize) -> Modal<'_> {
        let start = self.focus.saturating_sub(20);
        let end = (start + 41).min(self.options.len());
        let mut rows = Vec::new();
        for (i, (label, description)) in self.options.iter().enumerate().take(end).skip(start) {
            let state = if self.selected[i] { "x" } else { " " };
            rows.push(format!("[{state}] {label}  {description}"));
        }
        if end == self.options.len() {
            rows.push("Custom answer".into());
        }
        let mut modal = Modal::text(
            " Agent question ",
            format!("{}/{}  {}", index + 1, count, self.text),
        );
        modal.list = Some((rows, self.focus - start));
        modal.input =
            (self.focus == self.options.len()).then_some((&self.custom, "Type an answer…"));
        modal.hint = " ↑/↓ select · Space mark · Enter answer · Esc cancel ";
        modal
    }

    fn key(&mut self, key: KeyEvent) -> Option<Value> {
        let custom = self.focus == self.options.len();
        match key.code {
            KeyCode::Up if self.focus > 0 => self.focus -= 1,
            KeyCode::Down | KeyCode::Tab if !custom => self.focus += 1,
            KeyCode::Tab => self.focus = 0,
            KeyCode::Char(' ') if !custom => {
                let checked = self.selected[self.focus];
                if !self.multi {
                    self.selected.fill(false);
                }
                self.selected[self.focus] = !checked;
            }
            KeyCode::Enter => {
                if !custom && !self.selected.iter().any(|checked| *checked) {
                    self.selected[self.focus] = true;
                }
                let text = if custom || self.multi {
                    self.custom.text().trim()
                } else {
                    ""
                };
                let selected: Vec<_> = self
                    .options
                    .iter()
                    .zip(&self.selected)
                    .filter_map(|((label, _), checked)| {
                        (*checked && (self.multi || text.is_empty())).then_some(label)
                    })
                    .collect();
                if selected.is_empty() && text.is_empty() {
                    return None;
                }
                let mut answer = json!({"id":self.id,"selected":selected});
                if !text.is_empty() {
                    answer["custom"] = json!(text);
                }
                return Some(answer);
            }
            _ if custom => {
                edit_key(&mut self.custom, key, false);
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(method: &str, params: Value) -> Interaction {
        Interaction::from_request(
            &Request {
                jsonrpc: "2.0".into(),
                id: JsonRpcId::String("req_test".into()),
                method: method.into(),
                params: Some(params),
            },
            Instant::now(),
        )
        .unwrap()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn approval_defaults_to_deny_and_requires_explicit_choice_to_allow() {
        let mut modal = make(
            METHOD_APPROVAL_REQUEST,
            json!({"sessionId":"s","toolName":"read","reason":"Fixture approval"}),
        );
        assert!(modal.modal().body.contains("Fixture approval"));
        assert_eq!(
            modal.key(key(KeyCode::Enter)),
            Some(json!({"outcome":"rejected"}))
        );
        modal.key(key(KeyCode::Right));
        assert_eq!(
            modal.key(key(KeyCode::Enter)),
            Some(json!({"outcome":"allowed-once"}))
        );
        assert_eq!(
            modal.key(key(KeyCode::Esc)),
            Some(json!({"outcome":"cancelled"}))
        );
    }

    #[test]
    fn answers_multiple_questions_with_labels_and_custom_text() {
        let mut modal = make(
            METHOD_USER_QUESTION,
            json!({"requestId":"ask-1","sessionId":"s","questions":[
                {"id":"one","question":"Select two","options":[{"label":"A"},{"label":"B"}],"multiSelect":true},
                {"id":"two","question":"Type an answer"}
            ]}),
        );
        assert_eq!(modal.request_id(), Some("ask-1"));
        for code in [
            KeyCode::Char(' '),
            KeyCode::Down,
            KeyCode::Char(' '),
            KeyCode::Enter,
        ] {
            assert!(modal.key(key(code)).is_none());
        }
        modal.paste("自定义");
        assert_eq!(
            modal.key(key(KeyCode::Enter)),
            Some(
                json!({"answers":[{"id":"one","selected":["A","B"]},{"id":"two","selected":[],"custom":"自定义"}]})
            )
        );
    }

    #[test]
    fn custom_answer_replaces_a_single_selected_option() {
        let mut modal = make(
            METHOD_USER_QUESTION,
            json!({"requestId":"ask-2","questions":[
                {"id":"one","question":"Select one","options":[{"label":"A"}]}
            ]}),
        );
        assert!(modal.key(key(KeyCode::Char(' '))).is_none());
        modal.paste("Custom");
        assert_eq!(
            modal.key(key(KeyCode::Enter)),
            Some(json!({"answers":[{"id":"one","selected":[],"custom":"Custom"}]}))
        );
    }
}
