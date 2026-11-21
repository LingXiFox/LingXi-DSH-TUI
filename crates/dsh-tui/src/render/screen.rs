//! Compact transcript, draft editor, and modal presentation.

use dsh_state::TransportState;
use dsh_state::conversation::Conversation;
use dsh_state::draft::Draft;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::chat::{Viewport, safe_text};

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
    pub input: Option<&'a Draft>,
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
    let inset = u16::from(area.width > 32) * 2;
    let x = area.x + inset;
    let width = area.width - 2 * inset;
    let line = |y| Rect::new(x, area.y + y, width, 1);
    let header = Line::from(vec![
        Span::styled(
            "DSH",
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " {}  ·  {}  ·  {}",
            screen.version,
            safe_text(screen.route),
            if screen.running {
                "running"
            } else {
                screen.transport.label()
            }
        )),
    ]);
    frame.render_widget(header, line(0));
    if area.height == 1 {
        return;
    }
    let header_height = if area.height >= 7 { 2 } else { 1 };
    if header_height == 2 {
        frame.render_widget(
            Line::styled(
                safe_text(&format!("{}  ·  {}", screen.cwd, screen.session)),
                Style::default().fg(Color::Gray),
            ),
            line(1),
        );
    }
    let footer_height = u16::from(area.height >= 4);
    let draft_height = if area.height >= 12 { 3 } else { 1 };
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
                    .wrap(ratatui::widgets::Wrap { trim: false }),
                chat,
            );
        } else {
            frame.render_widget(Paragraph::new(lines), chat);
        }
    }
    let input = Rect::new(x, area.y + draft_y, width, draft_height);
    edit(
        frame,
        input,
        screen.draft,
        !viewport.focused && screen.modal.is_none(),
        "› ",
    );
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
        let keys = "Ctrl+P commands  ·  ? help";
        let footer = if screen.status.is_empty() {
            format!("{badge}  |  {keys}")
        } else {
            format!("{}  |  {badge}", screen.status)
        };
        frame.render_widget(
            Line::styled(safe_text(&footer), Style::default().fg(Color::Gray)),
            line(area.height - 1),
        );
    }
    if let Some(modal) = &screen.modal {
        overlay(frame, area, modal);
    }
}

fn overlay(frame: &mut Frame<'_>, area: Rect, modal: &Modal<'_>) {
    let rect = super::layout::centered(area, 74, 16);
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(modal.title)
        .border_style(Style::default().fg(Color::Cyan));
    let mut inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.is_empty() {
        return;
    }
    if let Some(draft) = modal.input {
        inner.height -= 1;
        edit(
            frame,
            Rect::new(inner.x, inner.bottom(), inner.width, 1),
            draft,
            true,
            "› ",
        );
    }
    frame.render_widget(
        Paragraph::new(safe_text(&modal.body)).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
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
                    Span::styled(marker.to_owned(), Style::default().fg(Color::Cyan)),
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
        let conversation = Conversation::default();
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
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        terminal.backend_mut().resize(9, 2);
        terminal
            .draw(|frame| draw(frame, &screen, &conversation, &mut viewport))
            .unwrap();
        assert_eq!(terminal.backend().buffer().area, Rect::new(0, 0, 9, 2));
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "D");
    }
}
