use super::*;
use crate::render::screen::{Screen, draw};
use dsh_protocol::protocol::{Notification, ServerNotification};
use dsh_state::{TransportState, draft::Draft};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn event(conversation: &mut Conversation, seq: u64, kind: &str, data: Value) {
    conversation
        .push(Notification {
            jsonrpc: "2.0".into(),
            method: ServerNotification::SessionEvent.as_str().into(),
            params: Some(json!({"sessionId":"s","event":{"seq":seq,"type":kind,"data":data}})),
        })
        .unwrap();
}

fn message(conversation: &mut Conversation, seq: u64, text: &str) {
    event(
        conversation,
        seq,
        "assistant/message",
        json!({"message":{"content":[{"type":"text","text":text}]}}),
    );
}

fn tool(conversation: &mut Conversation, seq: u64, id: String, body: &str, failed: bool) {
    event(
        conversation,
        seq,
        "tool/call",
        json!({"turn":1,"step":seq / 10,"callId":id,"name":"read","arguments":"{\"file_path\":\"fixture.txt\"}"}),
    );
    event(
        conversation,
        seq + 1,
        "tool/result",
        json!({"turn":1,"message":{"toolCallId":id,"isError":failed,"content":[{"type":"text","text":body}]}}),
    );
}

fn visible(view: &mut Viewport, conversation: &Conversation, width: u16, height: usize) -> String {
    view.lines("s", conversation, width, height)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn folds_keep_call_identity_across_modes_and_jump_to_failure() {
    let mut conversation = Conversation::default();
    tool(&mut conversation, 1, "first".into(), "LONG-RESULT-Ω", true);
    message(&mut conversation, 3, "assistant reply");
    let mut view = Viewport::default();
    let compact = visible(&mut view, &conversation, 90, 12);
    assert!(compact.contains("1 failed") && compact.contains("assistant reply"));
    assert!(!compact.contains("LONG-RESULT-Ω"));
    view.toggle("s", &conversation);
    let open = visible(&mut view, &conversation, 90, 12);
    assert!(open.contains("fixture.txt") && open.contains("first"));
    assert!(!open.contains("LONG-RESULT-Ω"));
    assert!(view.failure("s", &conversation));
    assert!(visible(&mut view, &conversation, 75, 12).contains("LONG-RESULT-Ω"));
    view.mode = Mode::Normal;
    assert!(visible(&mut view, &conversation, 75, 12).contains("LONG-RESULT-Ω"));
    view.toggle("s", &conversation);
    view.mode = Mode::Verbose;
    assert!(!visible(&mut view, &conversation, 75, 12).contains("LONG-RESULT-Ω"));
}

#[test]
fn visible_history_stays_anchored_until_follow_is_restored() {
    let mut conversation = Conversation::default();
    for i in 1..=100 {
        message(&mut conversation, i, &format!("message {i}"));
    }
    let mut view = Viewport::default();
    assert!(visible(&mut view, &conversation, 40, 6).contains("message 100"));
    view.scroll("s", &conversation, false, true);
    let before = visible(&mut view, &conversation, 40, 6);
    message(&mut conversation, 101, "new-arrival");
    assert!(!view.following());
    assert_eq!(visible(&mut view, &conversation, 40, 6), before);
    assert!(!visible(&mut view, &conversation, 24, 6).contains("new-arrival"));
    view.reset();
    assert!(visible(&mut view, &conversation, 24, 6).contains("new-arrival"));
    assert!(view.following());
}

#[test]
fn long_fenced_body_only_layouts_visible_chunks() {
    let mut conversation = Conversation::default();
    let code = format!(
        "```rust\n{}```\n| a | b |\n[label](https://example.invalid)",
        "let name = 3;\n".repeat(6_000)
    );
    message(&mut conversation, 1, &code);
    let mut view = Viewport::default();
    let lines = view.lines("s", &conversation, 90, 10);
    assert!(view.layouts.len() < 5);
    assert!(
        lines
            .iter()
            .any(|line| line.to_string().contains("│ a │ b │"))
    );
    assert!(
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
    );
    assert!(
        lines.iter().flat_map(|line| &line.spans).any(
            |span| span.content.contains("let name") && span.style.fg == Some(Color::LightCyan)
        )
    );
}

fn percentiles(samples: &mut [Duration]) -> [f64; 3] {
    samples.sort_unstable();
    [50, 95, 99].map(|pct| samples[(samples.len() * pct).div_ceil(100) - 1].as_secs_f64() * 1000.0)
}

#[test]
#[ignore = "run explicitly in release mode for performance measurements"]
fn benchmark_chat() {
    let mut conversation = match std::env::var_os("DSH_BENCH_ARCHIVE") {
        Some(directory) => {
            Conversation::open(std::path::Path::new(&directory), "benchmark").unwrap()
        }
        None => Conversation::default(),
    };
    let long = format!(
        "# Long answer\n\n```rust\n{}```\n",
        "let example = 1;\n".repeat(16_384)
    );
    let large = "output line with a fixed payload.\n".repeat(32_768);
    let start = Instant::now();
    let mut seq = 1;
    for _ in 0..88_000 {
        event(
            &mut conversation,
            seq,
            "step/start",
            json!({"turn":1,"step":seq}),
        );
        seq += 1;
    }
    for i in 0..10_000 {
        event(
            &mut conversation,
            seq,
            "assistant/message",
            json!({"stream":[{"type":"text-chunks","texts":["Reply ","with ","packed ","delta ",&i.to_string()]}]}),
        );
        seq += 1;
    }
    for i in 0..1_000 {
        tool(
            &mut conversation,
            seq,
            format!("tool-{i}"),
            if i == 999 { &large } else { "short output" },
            false,
        );
        seq += 2;
    }
    let ingest = start.elapsed();
    assert_eq!(conversation.received_count(), 100_000);
    let mut view = Viewport {
        mode: Mode::Verbose,
        ..Viewport::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
    let mut draft = Draft::default();
    let paint = |terminal: &mut Terminal<TestBackend>,
                 view: &mut Viewport,
                 draft: &Draft,
                 conversation: &Conversation| {
        let screen = Screen {
            cwd: "/workspace",
            session: "s",
            route: "fixture / model",
            version: "test",
            transport: &TransportState::Detached,
            running: false,
            status: "",
            draft,
            modal: None,
        };
        terminal
            .draw(|frame| draw(frame, &screen, conversation, view))
            .unwrap();
    };
    let mut frames = Vec::new();
    let mut scroll = Vec::new();
    let mut input = Vec::new();
    assert!(visible(&mut view, &conversation, 116, 30).contains("fixed payload"));
    for i in 0..240 {
        if i >= 48 && i % 12 == 0 {
            message(
                &mut conversation,
                seq,
                if i % 24 == 0 { &long } else { "small append" },
            );
            seq += 1;
            view.reset();
        }
        let start = Instant::now();
        paint(&mut terminal, &mut view, &draft, &conversation);
        frames.push(start.elapsed());
        let start = Instant::now();
        view.scroll("s", &conversation, i % 3 == 0, true);
        paint(&mut terminal, &mut view, &draft, &conversation);
        scroll.push(start.elapsed());
        let start = Instant::now();
        draft.insert("中e\u{301}");
        paint(&mut terminal, &mut view, &draft, &conversation);
        input.push(start.elapsed());
    }
    println!(
        "ingest_events=100000 elapsed_ms={:.2} events_per_s={:.0}",
        ingest.as_secs_f64() * 1000.0,
        100_000.0 / ingest.as_secs_f64()
    );
    for (name, mut samples) in [
        ("frame", frames),
        ("scroll_and_draw", scroll),
        ("edit_and_draw", input),
    ] {
        let [p50, p95, p99] = percentiles(&mut samples);
        println!("{name}_ms p50={p50:.3} p95={p95:.3} p99={p99:.3}");
    }
}
