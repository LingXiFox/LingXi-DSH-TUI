use super::*;
use dsh_protocol::protocol::{Notification, ServerNotification};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};
use std::fmt::Write as _;

#[test]
fn layout_goldens() {
    let mut conversation = Conversation::default();
    let mut seq = 0;
    let mut event = |kind: &str, data: Value| {
        seq += 1;
        conversation
            .push(Notification {
                jsonrpc: "2.0".into(),
                method: ServerNotification::SessionEvent.as_str().into(),
                params: Some(
                    json!({"sessionId":"golden","event":{"seq":seq,"type":kind,"data":data}}),
                ),
            })
            .unwrap();
    };
    event("session/title", json!({"title":"Review the archive"}));
    event(
        "model/selection",
        json!({"provider":"deepseek-official","model":"deepseek-v4-pro","reasoningEffort":"high"}),
    );
    event("request/context", json!({"contextWindow":128000}));
    event(
        "user/message",
        json!({"source":{"kind":"user"},"content":[{"type":"text","text":format!("{}仅展开后可见", "请检查这段归档，并逐项核对。\n".repeat(10))}]}),
    );
    for index in 0..12 {
        let call = format!("call-{index}");
        let tool = if index < 5 {
            "read"
        } else if index < 8 {
            "edit"
        } else {
            "bash"
        };
        event(
            "tool/call",
            json!({"turn":1,"callId":call,"name":tool,"arguments":"{\"file_path\":\"src/archive.rs\"}"}),
        );
        event(
            "tool/result",
            json!({"turn":1,"message":{"toolCallId":call,"content":[{"type":"text","text":"Checked archive."}]}}),
        );
    }
    event(
        "todo/write",
        json!({"todos":[{"content":"Check replay order","status":"completed"}]}),
    );
    event(
        "assistant/message",
        json!({
            "stream":[{"type":"reasoning-chunks","texts":["Hidden reasoning content"]}],
            "message":{"content":[{"type":"text","text":"## Findings\n\nReplay is **stable**.\n- Input order preserved\n- Duplicates ignored\n\n```rust\nlet archive = open()?;\n```"}]},
            "usage":{"inputTokens":2524,"outputTokens":204,"cacheReadTokens":2048,"cacheWriteTokens":0}
        }),
    );
    let mut draft = Draft::default();
    draft.insert("继续检查？\nKeep the source unchanged.");
    let empty = Draft::default();
    let inventory = InventorySummary {
        skills: Some(4),
        plugins: Some((8, 12)),
        mcp: Some(vec![("workspace-index".into(), "disabled".into())]),
    };
    let ready = TransportState::Ready {
        server_name: "fixture".into(),
        server_version: Some("1".into()),
    };
    for (width, height) in [(80, 24), (120, 36), (160, 45), (269, 70)] {
        let mut result = String::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut paint =
            |name, session, draft, sidebar, modal: Option<Modal<'_>>, view: &mut Viewport| {
                let focused = modal.is_some();
                let list = modal.as_ref().is_some_and(|value| value.list.is_some());
                let screen = Screen {
                    cwd: "/private/fixture/Archive",
                    branch: Some("review/archive"),
                    session,
                    route: "deepseek-official / deepseek-v4-pro",
                    transport: &ready,
                    running: false,
                    status: "",
                    draft,
                    modal,
                    sidebar,
                    inventory: &inventory,
                };
                terminal
                    .draw(|frame| draw(frame, &screen, &conversation, view))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                assert_eq!(buffer.area, Rect::new(0, 0, width, height));
                if list {
                    assert!(
                        buffer
                            .content
                            .iter()
                            .any(|cell| cell.bg == SELECTION && cell.symbol() == "›")
                    );
                }
                if focused {
                    assert!(buffer.content.iter().any(|cell| cell.bg == DIALOG));
                }
                writeln!(result, "=== {name} · {width}×{height} ===").unwrap();
                for row in buffer.content.chunks(usize::from(width)) {
                    let text = row.iter().map(|cell| cell.symbol()).collect::<String>();
                    writeln!(result, "{}", text.trim_end()).unwrap();
                }
            };
        paint(
            "Empty",
            "new",
            &empty,
            false,
            None,
            &mut Viewport::default(),
        );
        paint(
            "Chat",
            "golden",
            &draft,
            true,
            None,
            &mut Viewport::default(),
        );
        let mut view = Viewport::default();
        view.lines("golden", &conversation, width, height.into());
        view.toggle("golden", &conversation);
        paint("Expanded tools", "golden", &draft, true, None, &mut view);

        let (title, body, choices, selected) = match width {
            80 => (
                "Tool approval",
                "A command requires explicit approval.",
                vec!["Deny", "Allow once"],
                0,
            ),
            120 => (
                "History",
                "",
                vec![
                    "Inspect the archive",
                    "Review replay order",
                    "Check duplicate events",
                ],
                1,
            ),
            160 => (
                "Provider / Model",
                "",
                vec!["deepseek-v4-flash", "deepseek-v4-pro"],
                1,
            ),
            _ => (
                "User question",
                "Which evidence should be included?",
                vec!["[x] Replay order", "[ ] Duplicate events", "Submit answers"],
                0,
            ),
        };
        let mut modal = Modal::text(title, body);
        if body.is_empty() {
            modal.input = Some((&empty, "Filter…"));
        }
        modal.warning = width == 80;
        modal.list = Some((
            choices
                .into_iter()
                .map(|title| ListItem {
                    title: title.into(),
                    subtitle: if width == 120 {
                        "Archive · 2026-11-21".into()
                    } else {
                        String::new()
                    },
                    group: if width == 160 {
                        "DeepSeek".into()
                    } else {
                        String::new()
                    },
                    ..ListItem::default()
                })
                .collect(),
            selected,
        ));
        paint(
            title,
            "golden",
            &draft,
            false,
            Some(modal),
            &mut Viewport::default(),
        );
        assert!(!result.contains("仅 展 开 后 可 见"));
        assert!(!result.contains("Hidden reasoning content"));
        assert_eq!(result.contains("Workspace"), width >= 100);

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/goldens")
            .join(format!("{width}x{height}.txt"));
        if std::env::var_os("DSH_UPDATE_GOLDENS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &result).unwrap();
        }
        assert_eq!(
            result,
            std::fs::read_to_string(&path).unwrap(),
            "{}",
            path.display()
        );
    }
}
