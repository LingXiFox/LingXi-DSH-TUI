//! Compact transcript, draft editor, and modal presentation.

use dsh_state::TransportState;
use dsh_state::conversation::{Conversation, Session, TodoStatus};
use dsh_state::draft::Draft;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::chat::{Viewport, safe_text, wrap};
use super::theme::*;

pub struct Screen<'a> {
    pub cwd: &'a str,
    pub branch: Option<&'a str>,
    pub session: &'a str,
    pub route: &'a str,
    pub transport: &'a TransportState,
    pub running: bool,
    pub status: &'a str,
    pub draft: &'a Draft,
    pub modal: Option<Modal<'a>>,
    pub sidebar: bool,
    pub inventory: &'a InventorySummary,
}

#[derive(Default)]
pub struct InventorySummary {
    pub skills: Option<usize>,
    pub plugins: Option<(usize, usize)>,
    pub mcp: Option<Vec<(String, String)>>,
}

#[derive(Clone, Default)]
pub struct ListItem {
    pub title: String,
    pub subtitle: String,
    pub state: String,
    pub group: String,
}

impl From<String> for ListItem {
    fn from(title: String) -> Self {
        Self {
            title,
            ..Self::default()
        }
    }
}

impl From<&str> for ListItem {
    fn from(title: &str) -> Self {
        title.to_owned().into()
    }
}

pub struct Modal<'a> {
    pub title: &'a str,
    pub body: String,
    pub input: Option<(&'a Draft, &'a str)>,
    pub list: Option<(Vec<ListItem>, usize)>,
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
    frame
        .buffer_mut()
        .set_style(area, Style::default().bg(BACKGROUND).fg(TEXT));
    let gutter = u16::from(area.width >= 44) * 2;
    let side = if screen.sidebar && area.width >= 100 && area.height >= 12 {
        40
    } else {
        0
    };
    let width = (area.width - 2 * gutter - side).min(108);
    let x = area.x + (area.width - width - side) / 2;
    let line = |y| Rect::new(x, area.y + y, width, 1);
    let framed = area.height >= 10 && width >= 18;
    let session = conversation.sessions.get(screen.session);
    let status = if screen.running {
        "● Running"
    } else if screen.route == "Offline replay" {
        "Offline"
    } else {
        screen.transport.label()
    };
    let status_width = if width >= 36 {
        status.width() as u16 + 2
    } else {
        0
    };
    let title = session
        .and_then(|s| s.title.as_deref())
        .unwrap_or(if session.is_some() {
            "Untitled session"
        } else {
            "New session"
        });
    frame.render_widget(
        Line::from(vec![
            Span::styled("LingXi DSH", Style::default().fg(ACCENT).bold()),
            Span::styled(
                fit(
                    &format!("  /  {title}  ·  {}", project(screen.cwd)),
                    width.saturating_sub(status_width + 10),
                ),
                MUTED,
            ),
        ]),
        line(0),
    );
    if status_width > 0 {
        let color = if screen.running {
            ACCENT
        } else if matches!(screen.transport, TransportState::Failed(_)) {
            ERROR
        } else {
            MUTED
        };
        frame.render_widget(
            Line::styled(status, color).right_aligned(),
            Rect::new(x + width - status_width, area.y, status_width, 1),
        );
    }
    if area.height == 1 {
        if let Some(modal) = &screen.modal {
            overlay(frame, area, modal);
        }
        return;
    }
    let header_height = 1 + u16::from(area.height >= 12);
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
                    .fg(MUTED)
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
        composer(frame, input, screen, session, composer_focused);
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
        let keys = if width >= 81 {
            "Enter send · Ctrl+J newline · Ctrl+P commands · F7 sidebar"
        } else {
            "Ctrl+P commands · F7 sidebar"
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
    if side > 0 {
        draw_sidebar(
            frame,
            Rect::new(x + width + 2, area.y + 1, side - 2, area.height - 2),
            screen,
            session,
            conversation,
        );
    }
    if let Some(modal) = &screen.modal {
        overlay(frame, area, modal);
    }
}

pub fn project(cwd: &str) -> &str {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(if cwd.is_empty() { "No project" } else { cwd })
}

fn draw_sidebar(
    frame: &mut Frame<'_>,
    area: Rect,
    screen: &Screen<'_>,
    session: Option<&Session>,
    conversation: &Conversation,
) {
    let mut lines = Vec::with_capacity(usize::from(area.height));
    let mut section = |label: &str, values: &[String]| {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(
            fit(label, area.width - 3),
            Style::default().fg(TEXT).bold(),
        ));
        lines.extend(
            values
                .iter()
                .map(|line| Line::styled(fit(line, area.width - 3), MUTED)),
        );
    };
    section(
        "Workspace",
        &[
            project(screen.cwd).to_owned(),
            screen.branch.unwrap_or("Git branch unavailable").to_owned(),
        ],
    );
    if let Some(session) = session {
        let number = |n: Option<u64>| n.map_or_else(|| "—".into(), |n| n.to_string());
        section(
            "Context · last request",
            &[
                format!(
                    "Input {} / {}",
                    number(session.input_tokens),
                    number(session.context_window)
                ),
                format!("Output {}", number(session.output_tokens)),
            ],
        );
        if session.plan.is_some() || !session.todos.is_empty() {
            let mut todos = Vec::new();
            if let Some(active) = session.plan {
                todos.push(
                    if active {
                        "Plan mode on"
                    } else {
                        "Plan mode off"
                    }
                    .into(),
                );
            }
            for todo in session.todos.iter().take(5) {
                let mark = match todo.status {
                    TodoStatus::Pending => "○",
                    TodoStatus::InProgress => "◉",
                    TodoStatus::Completed => "✓",
                };
                if let Ok(text) = conversation.body(&todo.content) {
                    todos.push(format!(
                        "{mark} {}",
                        text.split('\n')
                            .next()
                            .unwrap_or_default()
                            .chars()
                            .take(100)
                            .collect::<String>()
                    ));
                }
            }
            if session.todos.len() > 5 {
                todos.push(format!("+{} more", session.todos.len() - 5));
            }
            section("Todo / Plan", &todos);
        }
        section(
            "Scoped writes",
            &[format!(
                "{} files · successful edits/writes",
                session.edited_paths.len()
            )],
        );
        if !session.children.is_empty() {
            let mut children = session
                .children
                .iter()
                .take(4)
                .map(|id| {
                    let child = conversation.sessions.get(id);
                    let title = child.and_then(|s| s.title.as_deref()).unwrap_or("Subagent");
                    let state = child.and_then(|s| s.outcome.as_deref()).unwrap_or(
                        if child.is_some_and(|s| s.running) {
                            "running"
                        } else {
                            "no final status"
                        },
                    );
                    format!("{title} · {state}")
                })
                .collect::<Vec<_>>();
            if session.children.len() > 4 {
                children.push(format!("+{} more", session.children.len() - 4));
            }
            section("Subagents", &children);
        }
    }
    let inventory = screen.inventory;
    if let Some(mcp) = &inventory.mcp {
        let mut states = mcp
            .iter()
            .take(4)
            .map(|(name, state)| format!("{name} · {state}"))
            .collect::<Vec<_>>();
        if mcp.len() > 4 {
            states.push(format!("+{} more", mcp.len() - 4));
        }
        if states.is_empty() {
            states.push("No configured servers".into());
        }
        section("MCP", &states);
    }
    let mut ecosystem = Vec::new();
    if let Some(skills) = inventory.skills {
        ecosystem.push(format!("{skills} skills"));
    }
    if let Some((enabled, total)) = inventory.plugins {
        ecosystem.push(format!("{enabled} / {total} plugins enabled"));
    }
    if !ecosystem.is_empty() {
        section("Ecosystem", &ecosystem);
    }
    let border = Block::default()
        .borders(Borders::LEFT)
        .border_style(BORDER)
        .padding(ratatui::widgets::Padding::left(2));
    let inner = border.inner(area);
    frame.render_widget(border, area);
    frame.render_widget(Paragraph::new(lines), inner);
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
    let wide = modal.list.as_ref().is_some_and(|(rows, _)| {
        rows.iter()
            .any(|row| !row.subtitle.is_empty() || !row.group.is_empty())
    });
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
        for (index, item) in rows.iter().enumerate().skip(start).take(count) {
            let selected = index == *selected;
            let row = Rect {
                y: inner.y + (index - start) as u16,
                height: 1,
                ..inner
            };
            let style = if selected {
                Style::default().fg(accent).bg(SELECTION).bold()
            } else {
                Style::default().fg(TEXT)
            };
            frame.buffer_mut().set_style(row, style);
            let right = if row.width >= 24 && !item.state.is_empty() {
                (item.state.width() as u16 + 2).min(row.width / 3)
            } else {
                0
            };
            let text_width = row.width.saturating_sub(right + 2);
            let secondary = match (item.group.as_str(), item.subtitle.as_str()) {
                ("", value) | (value, "") => value.to_owned(),
                (group, subtitle) => format!("{group} · {subtitle}"),
            };
            let secondary_width = if text_width >= 23 && !secondary.is_empty() {
                (secondary.width() as u16 + 3).min(text_width / 2)
            } else {
                0
            };
            let mut parts = vec![
                Span::styled(if selected { "› " } else { "  " }, style),
                Span::styled(fit(&item.title, text_width - secondary_width), style),
            ];
            if secondary_width > 0 {
                parts.push(Span::styled(
                    format!("   {}", fit(&secondary, secondary_width - 3)),
                    MUTED,
                ));
            }
            frame.render_widget(Line::from(parts), row);
            if right > 0 {
                frame.render_widget(
                    Line::styled(fit(&item.state, right - 1), MUTED).right_aligned(),
                    Rect::new(row.right() - right, row.y, right - 1, 1),
                );
            }
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
#[path = "screen_golden.rs"]
mod golden;

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
            branch: None,
            session: "session-test",
            route: "provider / model",
            transport: &TransportState::Failed("broken pipe".into()),
            running: false,
            status: "",
            draft: &draft,
            modal: None,
            sidebar: false,
            inventory: &InventorySummary::default(),
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
            "workspace",
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
        picker.list = Some((
            (0..41).map(|i| format!("Entry {i:02}").into()).collect(),
            30,
        ));
        let screen = Screen {
            cwd: "/workspace",
            branch: None,
            session: "test",
            route: "provider / model",
            transport: &TransportState::Detached,
            running: false,
            status: "",
            draft: &draft,
            modal: Some(picker),
            sidebar: false,
            inventory: &InventorySummary::default(),
        };
        let mut viewport = Viewport::default();
        for (width, height) in [(180, 18), (32, 7), (9, 5)] {
            terminal.backend_mut().resize(width, height);
            terminal
                .draw(|frame| draw(frame, &screen, &Conversation::default(), &mut viewport))
                .unwrap();
            let buffer = terminal.backend().buffer();
            if width == 180 {
                let logo = &buffer[(36, 0)];
                assert_eq!(logo.symbol(), "L");
                assert!(logo.modifier.contains(Modifier::DIM));
                assert_eq!(logo.bg, VEIL);
                assert!(buffer.content.iter().any(|cell| cell.bg == DIALOG));
            }
            let row: String = buffer
                .content
                .iter()
                .filter(|cell| cell.bg == SELECTION)
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
