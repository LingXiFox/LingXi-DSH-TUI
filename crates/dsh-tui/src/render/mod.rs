//! Rendering: a pure `draw(frame, state)` over `dsh-state` types.

pub mod keymap;
pub mod layout;

use dsh_state::{ActivityKind, AppState, FoldPolicy, TransportState};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

/// Indent used for nested activity rows.
const GUTTER: &str = "  ";

/// Draw the whole screen for the current state.
pub fn draw(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, rows[0], state);
    draw_body(frame, rows[1], state);
    draw_footer(frame, rows[2], state);

    if state.help_open {
        draw_help(frame, area);
    }
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let status_color = match state.transport {
        TransportState::Detached => Color::DarkGray,
        TransportState::Connecting => Color::Yellow,
        TransportState::Ready { .. } => Color::Green,
        TransportState::Failed(_) => Color::Red,
    };
    let title = Line::from(vec![
        Span::styled(
            " LingXi DSH-TUI ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(state.header_status(), Style::default().fg(status_color)),
    ]);
    let identity = Line::from(Span::styled(
        format!(" {}", state.identity_line()),
        Style::default().fg(Color::Gray),
    ));
    let paragraph = Paragraph::new(vec![title, identity]).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(paragraph, area);
}

fn draw_body(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    // Narrow terminals stack the panels instead of squeezing two columns.
    if !layout::is_usable(area) {
        let rows =
            Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
        draw_activity(frame, rows[0], state);
        draw_side(frame, rows[1], state);
        return;
    }
    let columns = Layout::horizontal([Constraint::Min(40), Constraint::Length(42)]).split(area);
    draw_activity(frame, columns[0], state);
    draw_side(frame, columns[1], state);
}

fn draw_activity(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let height = area.height.saturating_sub(2) as usize;
    let items = state
        .activity
        .tail(height.max(1))
        .map(|entry| {
            let color = match entry.kind {
                ActivityKind::Lifecycle => Color::Cyan,
                ActivityKind::Runtime => Color::Magenta,
                ActivityKind::Transport => Color::Blue,
                ActivityKind::Warning => Color::Yellow,
            };
            ListItem::new(Line::from(vec![
                Span::raw(GUTTER),
                Span::styled(
                    entry.kind.badge(),
                    Style::default().fg(color).add_modifier(Modifier::DIM),
                ),
                Span::raw("  "),
                Span::raw(
                    entry
                        .render()
                        .trim_start_matches(entry.kind.badge())
                        .trim()
                        .to_owned(),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let title = format!(
        " Activity  {} rows{} ",
        state.activity.len(),
        if state.activity.folded_count() > 0 {
            format!(", {} folded", state.activity.folded_count())
        } else {
            String::new()
        }
    );
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn draw_side(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let rows = Layout::vertical([Constraint::Length(8), Constraint::Min(3)]).split(area);
    let details = vec![
        Line::from(vec![
            Span::styled(" TUI      ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.tui_version),
        ]),
        Line::from(vec![
            Span::styled(" Agent    ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.agent_version),
        ]),
        Line::from(vec![
            Span::styled(" Tag      ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.agent_tag),
        ]),
        Line::from(vec![
            Span::styled(" Commit   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.agent_commit_short),
        ]),
        Line::from(vec![
            Span::styled(" Build    ", Style::default().fg(Color::DarkGray)),
            Span::raw(&state.build_id),
        ]),
        Line::from(vec![
            Span::styled(" Folding  ", Style::default().fg(Color::DarkGray)),
            Span::raw(match state.fold {
                FoldPolicy::Expanded => "expanded",
                FoldPolicy::Collapsed => "collapsed",
            }),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(details).block(Block::default().borders(Borders::ALL).title(" Runtime ")),
        rows[0],
    );

    let visible = state.visible_transcript();
    let hidden = state.transcript.len() - visible.len();
    let lines = if visible.is_empty() {
        vec![
            Line::from(Span::styled(
                " No transcript yet. ",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                " SDK connection status appears in Activity. ",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        visible
            .iter()
            .map(|entry| Line::from(format!(" {}  {}", entry.speaker, entry.body)))
            .collect()
    };
    let title = if hidden > 0 {
        format!(" Transcript   {hidden} folded ")
    } else {
        " Transcript ".to_owned()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        rows[1],
    );
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let hint = if state.help_open {
        " ? / Esc close help "
    } else {
        " q quit  ? help  f fold "
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Left),
        area,
    );
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
    let popup = layout::centered(area, 56, 14);
    frame.render_widget(Clear, popup);
    let lines = vec![
        Line::from(Span::styled(
            " LingXi DSH-TUI keymap ",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  q / Esc / Ctrl-C   quit"),
        Line::from("  ?                  toggle this help"),
        Line::from("  f                  fold or expand tool rows"),
        Line::from(""),
        Line::from(Span::styled(
            " Round 1 scope ",
            Style::default().fg(Color::Yellow),
        )),
        Line::from("  UI skeleton only: no Agent process, no chat, no provider."),
        Line::from("  The runtime pin is still verified at startup."),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Help "),
        ),
        popup,
    );
}

#[cfg(test)]
mod tests;
