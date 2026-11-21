//! Compact transcript, draft editor, and modal presentation.

use dsh_state::TransportState;
use dsh_state::conversation::{Conversation, Session};
use dsh_state::draft::Draft;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::chat::{Viewport, safe_text, wrap};

pub(super) const ACCENT: Color = Color::Rgb(119, 206, 193);
const TEXT: Color = Color::Rgb(222, 228, 234);
const MUTED: Color = Color::Rgb(151, 161, 172);
const SURFACE: Color = Color::Rgb(31, 37, 45);
const DIALOG: Color = Color::Rgb(24, 29, 35);
const FIELD: Color = Color::Rgb(37, 44, 53);
const BORDER: Color = Color::Rgb(74, 89, 100);
const VEIL: Color = Color::Rgb(9, 12, 15);
const VEILED_TEXT: Color = Color::Rgb(92, 101, 108);
const WARNING: Color = Color::Rgb(230, 187, 109);

pub struct Screen<'a> {
    pub cwd: &'a str,
    pub session: &'a str,
    pub route: &'a str,
    pub version: &'a str,
    pub transport: &'a TransportState,
    pub running: bool,
    pub status: &'a str,
    pub draft: &'a Draft,
    pub modal: Option<Modal<'a>>,
}

pub struct Modal<'a> {
    pub title: &'a str,
    pub body: String,
    pub input: Option<(&'a Draft, &'a str)>,
    pub list: Option<(Vec<String>, usize)>,
    pub hint: &'a str,
    pub scroll: u16,
    pub warning: bool,
}

impl<'a> Modal<'a> {
    pub fn text(title: &'a str, body: impl Into<String>) -> Self {
        Self {
            title,
            body: body.into(),
            input: None,
            list: None,
            hint: "PgUp/PgDn scroll · Esc close",
            scroll: 0,
            warning: false,
        }
    }
}

pub fn draw(
    frame: &mut Frame<'_>,
    screen: &Screen<'_>,
    conversation: &Conversation,
    viewport: &mut Viewport,
) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    let gutter = u16::from(area.width >= 44) * 2;
    let width = (area.width - 2 * gutter).min(104);
    let x = area.x + (area.width - width) / 2;
    let line = |y| Rect::new(x, area.y + y, width, 1);
    let framed = area.height >= 10 && width >= 18;
    let mut header = vec![
        Span::styled("LingXi", Style::default().fg(ACCENT).bold()),
        Span::styled(
            format!(
                "  · {}",
                if screen.running {
                    "running"
                } else {
                    screen.transport.label()
                }
            ),
            Style::default().fg(MUTED),
        ),
    ];
    if !framed {
        header.push(Span::raw(format!("   {}", safe_text(screen.route))));
    }
    frame.render_widget(Line::from(header), line(0));
    if area.height == 1 {
        if let Some(modal) = &screen.modal {
            overlay(frame, area, modal);
        }
        return;
    }
    let header_height = if area.height >= 14 {
        3
    } else if area.height >= 7 {
        2
    } else {
        1
    };
    if header_height >= 2 {
        frame.render_widget(
            Line::styled(
                fit(&format!("{}  ·  DSH {}", screen.cwd, screen.version), width),
                MUTED,
            ),
            line(1),
        );
    }
    let footer_height = u16::from(area.height >= 4);
    let draft_height = if framed {
        let rows = draft_rows(screen.draft, width - 2);
        3 + if area.height >= 14 {
            rows.min((area.height / 5).clamp(2, 5))
        } else {
            1
        }
    } else if area.height >= 8 {
        2
    } else {
        1
    };
    let draft_y = area.height - draft_height - footer_height;
    let chat = Rect::new(
        x,
        area.y + header_height,
        width,
        draft_y.saturating_sub(header_height),
    );
    let lines = viewport.lines(
        screen.session,
        conversation,
        chat.width,
        usize::from(chat.height),
    );
    if !chat.is_empty() {
        if lines.is_empty() {
            let hint = match screen.transport {
                TransportState::Failed(error) => format!("SDK failed · {error}"),
                TransportState::Ready { .. } => "Enter a prompt to start this session.".into(),
                _ => "Waiting for the bundled SDK…".into(),
            };
            frame.render_widget(
                Paragraph::new(safe_text(&hint))
                    .fg(Color::Gray)
                    .wrap(Wrap { trim: false }),
                chat,
            );
        } else {
            frame.render_widget(Paragraph::new(lines), chat);
        }
    }
    let input = Rect::new(x, area.y + draft_y, width, draft_height);
    let composer_focused = !viewport.focused && screen.modal.is_none();
    if framed {
        composer(
            frame,
            input,
            screen,
            conversation.sessions.get(screen.session),
            composer_focused,
        );
    } else {
        edit(frame, input, screen.draft, composer_focused, "› ");
    }
    if footer_height > 0 {
        let position = if viewport.following() {
            "tail"
        } else {
            "history"
        };
        let focus = if viewport.focused {
            "transcript"
        } else {
            "prompt"
        };
        let badge = format!("{}  ·  {position}  ·  {focus}", viewport.mode.label());
        let keys = if width >= 72 {
            "Enter send · Ctrl+J newline · Ctrl+P commands"
        } else {
            "Ctrl+P commands"
        };
        let status = if screen.status.is_empty() {
            keys
        } else {
            screen.status
        };
        frame.render_widget(
            Line::from(vec![
                Span::styled(badge, ACCENT),
                Span::raw("   "),
                Span::styled(safe_text(status), MUTED),
            ]),
            line(area.height - 1),
        );
    }
    if let Some(modal) = &screen.modal {
        overlay(frame, area, modal);
    }
}

fn composer(
    frame: &mut Frame<'_>,
    area: Rect,
    screen: &Screen<'_>,
    session: Option<&Session>,
    focused: bool,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused { ACCENT } else { BORDER })
        .style(Style::default().bg(SURFACE).fg(TEXT));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let input = Rect {
        height: inner.height - 1,
        ..inner
    };
    if screen.draft.text().is_empty() {
        frame.render_widget(
            Line::styled("Write a prompt…", MUTED),
            Rect::new(input.x + 2, input.y, input.width - 2, 1),
        );
    }
    edit(frame, input, screen.draft, focused, "› ");

    let recorded = session.and_then(|session| session.route.as_ref());
    let recorded_label = recorded.map(|route| format!("{} / {}", route.provider, route.model));
    let offline = screen.route == "Offline replay";
    let matching = offline || recorded_label.as_deref() == Some(screen.route);
    let route = if offline {
        recorded_label.as_deref().unwrap_or(screen.route)
    } else {
        screen.route
    };
    let effort = recorded
        .filter(|_| matching)
        .and_then(|route| route.reasoning_effort.as_deref())
        .unwrap_or(if offline { "—" } else { "default" });
    let tokens = |value: Option<u64>| value.map_or_else(|| "—".into(), |n| n.to_string());
    let cache = session.and_then(|session| session.cache.as_ref());
    let cache = match cache {
        Some(cache) => format!(
            "Cache last: read {} / write {}",
            tokens(cache.read),
            tokens(cache.write)
        ),
        None => "Cache last: —".into(),
    };
    let meta = Rect::new(inner.x + 2, inner.bottom() - 1, inner.width - 4, 1);
    let effort = format!("Reasoning: {}", safe_text(effort).replace('\n', " "));
    let sep = "  ·  ";
    let available = usize::from(meta.width);
    let extras = effort.width() + cache.width() + 2 * sep.width();
    let mut parts = Vec::with_capacity(5);
    if available >= extras + 12 {
        parts.push(Span::styled(fit(route, (available - extras) as u16), TEXT));
        parts.extend([
            Span::styled(sep, MUTED),
            Span::styled(effort, WARNING),
            Span::styled(sep, MUTED),
            Span::styled(cache, MUTED),
        ]);
    } else if available >= effort.width() + sep.width() + 12 {
        let route_width = available - effort.width() - sep.width();
        parts.push(Span::styled(fit(route, route_width as u16), TEXT));
        parts.push(Span::styled(sep, MUTED));
        parts.push(Span::styled(effort, WARNING));
    } else {
        parts.push(Span::styled(fit(route, meta.width), TEXT));
    }
    frame.render_widget(Line::from(parts), meta);
}

fn overlay(frame: &mut Frame<'_>, area: Rect, modal: &Modal<'_>) {
    frame.buffer_mut().set_style(
        area,
        Style::default()
            .bg(VEIL)
            .fg(VEILED_TEXT)
            .add_modifier(Modifier::DIM)
            .remove_modifier(Modifier::REVERSED),
    );
    let wide = modal
        .list
        .as_ref()
        .is_some_and(|(rows, _)| rows.iter().take(24).any(|row| row.width() > 60));
    let width = area
        .width
        .saturating_sub(u16::from(area.width >= 34) * 4)
        .min(if wide || modal.warning { 84 } else { 60 });
    let pad = if width >= 30 {
        3
    } else {
        u16::from(width >= 12)
    };
    let content_width = width.saturating_sub(2 * pad);
    let height = area
        .height
        .saturating_sub(u16::from(area.height >= 10) * 2)
        .min(28);
    let lines = if modal.body.is_empty() || content_width == 0 {
        Vec::new()
    } else {
        wrap(&modal.body, usize::from(content_width), false, false, true)
    };
    let body_lines = lines.len().min(usize::from(u16::MAX)) as u16;
    let body = Paragraph::new(lines);
    let roomy = area.height >= 13;
    let footer = height >= 5;
    let reserved = 1 + 3 * u16::from(roomy) + u16::from(modal.input.is_some()) + u16::from(footer);
    let list_lines = modal
        .list
        .as_ref()
        .map_or(0, |(rows, _)| rows.len().clamp(1, 20) as u16);
    let rect = super::layout::centered(
        area,
        width,
        (body_lines.saturating_add(list_lines + reserved)).clamp(1, height),
    );
    frame.render_widget(Clear, rect);
    let accent = if modal.warning { WARNING } else { ACCENT };
    frame.render_widget(
        Block::default().style(Style::default().bg(DIALOG).fg(TEXT)),
        rect,
    );
    let mut inner = rect;
    inner.x += pad;
    inner.width = content_width;
    let top = u16::from(roomy && inner.height >= 4);
    inner.y += top;
    inner.height -= top;
    if inner.height > 0 {
        let close = u16::from(inner.width >= 13) * 5;
        frame.render_widget(
            Line::styled(
                fit(modal.title.trim(), inner.width - close),
                Style::default()
                    .fg(if modal.warning { WARNING } else { TEXT })
                    .bold(),
            ),
            Rect {
                height: 1,
                width: inner.width - close,
                ..inner
            },
        );
        if close > 0 {
            frame.render_widget(
                Line::styled("esc", MUTED).right_aligned(),
                Rect::new(inner.right() - close, inner.y, close, 1),
            );
        }
        inner.y += 1;
        inner.height -= 1;
    }
    let bottom = u16::from(roomy && inner.height >= 5);
    if footer && inner.height >= 3 {
        inner.height -= 1 + bottom;
        frame.render_widget(
            Line::styled(fit(modal.hint, inner.width), MUTED),
            Rect {
                y: inner.bottom(),
                height: 1,
                ..inner
            },
        );
    }
    if let Some((draft, placeholder)) = modal.input.filter(|_| inner.height > 1) {
        let search = Rect { height: 1, ..inner };
        frame
            .buffer_mut()
            .set_style(search, Style::default().bg(FIELD));
        if draft.text().is_empty() && search.width > 3 {
            frame.render_widget(
                Line::styled(fit(placeholder, search.width - 2), MUTED),
                Rect {
                    x: search.x + 2,
                    width: search.width - 2,
                    ..search
                },
            );
        }
        edit(frame, search, draft, true, "› ");
        inner.y += 1;
        inner.height -= 1;
    }
    if roomy && inner.height >= 4 {
        inner.y += 1;
        inner.height -= 1;
    }
    let text_height = body_lines.min(if modal.list.is_some() {
        inner.height / 2
    } else {
        inner.height
    });
    if text_height > 0 {
        frame.render_widget(
            body.scroll((modal.scroll.min(body_lines - text_height), 0)),
            Rect {
                height: text_height,
                ..inner
            },
        );
        inner.y += text_height;
        inner.height -= text_height;
    }
    if let Some((rows, selected)) = &modal.list {
        let count = usize::from(inner.height);
        let start = selected
            .saturating_sub(count / 2)
            .min(rows.len().saturating_sub(count));
        for (index, label) in rows.iter().enumerate().skip(start).take(count) {
            let selected = index == *selected;
            let row = Rect {
                y: inner.y + (index - start) as u16,
                height: 1,
                ..inner
            };
            let style = if selected {
                Style::default().fg(VEIL).bg(accent).bold()
            } else {
                Style::default().fg(TEXT)
            };
            frame.buffer_mut().set_style(row, style);
            frame.render_widget(
                Line::styled(
                    fit(
                        &format!("{} {label}", if selected { "›" } else { " " }),
                        row.width,
                    ),
                    style,
                ),
                row,
            );
        }
    }
}

fn draft_rows(draft: &Draft, width: u16) -> u16 {
    let width = usize::from(width.saturating_sub(3).max(1));
    draft
        .text()
        .split('\n')
        .map(|line| (line.width() + 1).div_ceil(width))
        .sum::<usize>()
        .clamp(2, 5) as u16
}

fn fit(text: &str, width: u16) -> String {
    let text = safe_text(text).replace('\n', " ");
    let width = usize::from(width);
    if text.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let left = (width - 1) * 3 / 4;
    let right = width - 1 - left;
    let take = |unit: &&str, cells: &mut usize| {
        let size = unit.width();
        if size > *cells {
            false
        } else {
            *cells -= size;
            true
        }
    };
    let mut left_cells = left;
    let mut right_cells = right;
    let mut result: String = text
        .graphemes(true)
        .take_while(|unit| take(unit, &mut left_cells))
        .collect();
    let suffix: Vec<_> = text
        .graphemes(true)
        .rev()
        .take_while(|unit| take(unit, &mut right_cells))
        .collect();
    result.push('…');
    result.extend(suffix.into_iter().rev());
    result
}

fn edit(frame: &mut Frame<'_>, area: Rect, draft: &Draft, focused: bool, prefix: &str) {
    if area.is_empty() {
        return;
    }
    let prefix_width = prefix
        .width()
        .min(usize::from(area.width.saturating_sub(1)));
    let width = usize::from(area.width) - prefix_width;
    let selected = draft.selection();
    let mut lines = vec![Line::default()];
    let mut col = 0;
    let mut cursor = (0, 0);
    for (index, unit) in draft.text().grapheme_indices(true) {
        let value = if unit == "\t" { " " } else { unit };
        let cell_width = value.width();
        if unit == "\n" {
            if index == draft.cursor() {
                cursor = (lines.len() - 1, col);
            }
            lines.push(Line::default());
            col = 0;
            continue;
        }
        if col > 0 && col + cell_width > width {
            lines.push(Line::default());
            col = 0;
        }
        if index == draft.cursor() {
            cursor = (lines.len() - 1, col);
        }
        if cell_width <= width {
            let mut style = Style::default();
            if selected.contains(&index) {
                style = style.add_modifier(Modifier::REVERSED);
            }
            if let Some(line) = lines.last_mut() {
                line.spans.push(Span::styled(safe_text(value), style));
            }
            col += cell_width;
        }
    }
    if draft.cursor() == draft.text().len() {
        if col == width {
            lines.push(Line::default());
            col = 0;
        }
        cursor = (lines.len() - 1, col);
    }
    let top = cursor.0.saturating_sub(usize::from(area.height) - 1);
    let visible = lines
        .into_iter()
        .skip(top)
        .take(usize::from(area.height))
        .enumerate()
        .map(|(index, mut line)| {
            if prefix_width > 0 {
                let marker = if index == 0 {
                    if prefix_width == 1 { "›" } else { prefix }
                } else if prefix_width == 1 {
                    " "
                } else {
                    "  "
                };
                line.spans.insert(
                    0,
                    Span::styled(marker.to_owned(), Style::default().fg(ACCENT)),
                );
            }
            line
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), area);
    if focused {
        frame.set_cursor_position((
            area.x + (prefix_width + cursor.1.min(width - 1)) as u16,
            area.y + (cursor.0 - top) as u16,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn renders_unicode_draft_and_transport_failure_after_resize() {
        let mut terminal = Terminal::new(TestBackend::new(112, 16)).unwrap();
        let mut draft = Draft::default();
        draft.insert("你好\n组合 e\u{301}");
        let mut conversation = Conversation::default();
        use dsh_protocol::protocol::{Notification, ServerNotification};
        use serde_json::json;
        for (seq, kind, data) in [
            (
                1,
                "request/header",
                json!({"header":{"config":{"provider":"provider","model":"model","reasoningEffort":"high"}}}),
            ),
            (
                2,
                "assistant/message",
                json!({"usage":{"cacheReadTokens":4096,"cacheWriteTokens":0}}),
            ),
        ] {
            conversation.push(Notification {
                jsonrpc: "2.0".into(),
                method: ServerNotification::SessionEvent.as_str().into(),
                params: Some(json!({"sessionId":"session-test","event":{"type":kind,"seq":seq,"data":data}})),
            }).unwrap();
        }
        let mut viewport = Viewport::default();
        let screen = Screen {
            cwd: "/workspace",
            session: "session-test",
            route: "provider / model",
            version: "0.2.2-rc.5",
            transport: &TransportState::Failed("broken pipe".into()),
            running: false,
            status: "",
            draft: &draft,
            modal: None,
        };
        terminal
            .draw(|frame| draw(frame, &screen, &conversation, &mut viewport))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for expected in [
            "/workspace",
            "provider / model",
            "broken pipe",
            "你 好",
            "e\u{301}",
            "Compact",
            "Reasoning: high",
            "Cache last: read 4096 / write 0",
            "╭",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        terminal.backend_mut().resize(9, 2);
        terminal
            .draw(|frame| draw(frame, &screen, &conversation, &mut viewport))
            .unwrap();
        assert_eq!(terminal.backend().buffer().area, Rect::new(0, 0, 9, 2));
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "L");
    }

    #[test]
    fn centers_reading_column_and_keeps_scrolled_selection_visible() {
        let mut terminal = Terminal::new(TestBackend::new(180, 18)).unwrap();
        let draft = Draft::default();
        let mut picker = Modal::text(" History ", "");
        picker.input = Some((&draft, "Filter…"));
        picker.list = Some(((0..41).map(|i| format!("Entry {i:02}")).collect(), 30));
        let screen = Screen {
            cwd: "/workspace",
            session: "test",
            route: "provider / model",
            version: "test",
            transport: &TransportState::Detached,
            running: false,
            status: "",
            draft: &draft,
            modal: Some(picker),
        };
        let mut viewport = Viewport::default();
        for (width, height) in [(180, 18), (32, 7), (9, 5)] {
            terminal.backend_mut().resize(width, height);
            terminal
                .draw(|frame| draw(frame, &screen, &Conversation::default(), &mut viewport))
                .unwrap();
            let buffer = terminal.backend().buffer();
            if width == 180 {
                let logo = &buffer[(38, 0)];
                assert_eq!(logo.symbol(), "L");
                assert!(logo.modifier.contains(Modifier::DIM));
                assert_eq!(logo.bg, VEIL);
                assert!(buffer.content.iter().any(|cell| cell.bg == DIALOG));
            }
            let row: String = buffer
                .content
                .iter()
                .filter(|cell| cell.bg == ACCENT)
                .map(|cell| cell.symbol())
                .collect();
            assert!(row.starts_with('›'), "no selected row at {width}×{height}");
            if width >= 32 {
                assert!(row.contains("Entry 30"));
            }
        }
    }

    #[test]
    fn long_approval_details_scroll_without_hiding_selected_action() {
        let mut terminal = Terminal::new(TestBackend::new(38, 9)).unwrap();
        let mut dialog = Modal::text(
            " Tool approval ",
            format!("{}\nEnd of reason", "line of detail\n".repeat(10)),
        );
        dialog.list = Some((vec!["Deny".into(), "Allow once".into()], 0));
        dialog.warning = true;
        dialog.scroll = u16::MAX;
        terminal
            .draw(|frame| overlay(frame, frame.area(), &dialog))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("End of reason"));
        assert!(text.contains("› Deny"));
        assert!(text.contains("Allow once"));
    }
}
