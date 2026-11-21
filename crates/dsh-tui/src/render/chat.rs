//! Viewport traversal and bounded, lazy text layout.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Weak};

use dsh_state::conversation::{Conversation, Row, Session};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
use unicode_width::UnicodeWidthStr;

const CHUNK_BYTES: usize = 4096;
const CACHE_SIZE: usize = 96;
const LAYOUT_BUDGET: usize = 4 * 1024 * 1024;
const FENCE_BUDGET: usize = 1024 * 1024;

#[derive(Clone, Copy, Default)]
pub enum Mode {
    #[default]
    Compact,
    Normal,
    Verbose,
}

impl Mode {
    pub fn next(self) -> Self {
        match self {
            Self::Compact => Self::Normal,
            Self::Normal => Self::Verbose,
            Self::Verbose => Self::Compact,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Normal => "Normal",
            Self::Verbose => "Verbose",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Position {
    row: usize,
    part: usize,
    chunk: usize,
    line: usize,
}

#[derive(Default)]
pub struct Viewport {
    pub mode: Mode,
    pub focused: bool,
    top: Option<Position>,
    selected: Position,
    painted: Vec<Position>,
    groups: HashMap<(String, u64), bool>,
    tools: HashMap<(String, u64, String), bool>,
    children: HashSet<(String, String)>,
    layouts: VecDeque<Layout>,
    fences: VecDeque<(Weak<String>, Vec<usize>)>,
    layout_bytes: usize,
    fence_bytes: usize,
    generations: HashMap<String, u64>,
    size: (u16, usize),
}

struct Layout {
    body: Weak<String>,
    chunk: usize,
    width: u16,
    markdown: bool,
    lines: Vec<Line<'static>>,
    bytes: usize,
}

struct Piece {
    body: Arc<String>,
    color: Color,
    markdown: bool,
}

impl Piece {
    fn new(body: impl Into<Arc<String>>, color: Color) -> Self {
        Self {
            body: body.into(),
            color,
            markdown: false,
        }
    }
}

impl Viewport {
    pub fn reset(&mut self) {
        self.top = None;
        self.selected = Position::default();
        self.painted.clear();
    }

    pub fn following(&self) -> bool {
        self.top.is_none()
    }

    pub fn home(&mut self) {
        self.top = Some(Position::default());
        self.selected = Position::default();
    }

    pub fn toggle(&mut self, id: &str, conversation: &Conversation) {
        let Some(session) = conversation.sessions.get(id) else {
            return;
        };
        if !self.focused
            && !matches!(
                session.rows.get(self.selected.row),
                Some(Row::Tools(_) | Row::Child(_))
            )
        {
            if let Some(pos) = self.painted.iter().rev().find(|pos| {
                pos.part == 0
                    && matches!(
                        session.rows.get(pos.row),
                        Some(Row::Tools(_) | Row::Child(_))
                    )
            }) {
                self.selected = *pos;
            }
        }
        self.focused = true;
        let pos = self.selected;
        match session.rows.get(pos.row) {
            Some(Row::Tools(turn)) => {
                let group = (id.to_owned(), *turn);
                if pos.part == 0 {
                    self.groups.insert(group, !self.group_open(id, *turn));
                    self.selected = Position {
                        row: pos.row,
                        ..Position::default()
                    };
                } else if let Some(tool) = session.turns[turn].tools.get((pos.part - 1) / 4) {
                    let key = (id.to_owned(), *turn, tool.id.clone());
                    let open = self
                        .tools
                        .get(&key)
                        .copied()
                        .unwrap_or(matches!(self.mode, Mode::Verbose));
                    self.tools.insert(key, !open);
                    self.selected = Position {
                        row: pos.row,
                        part: (pos.part - 1) / 4 * 4 + 1,
                        ..Position::default()
                    };
                }
                self.top = Some(self.selected);
            }
            Some(Row::Child(child)) => {
                flip(&mut self.children, (id.to_owned(), child.clone()));
                self.top = Some(Position {
                    row: pos.row,
                    ..Position::default()
                });
            }
            _ => {}
        }
    }

    pub fn failure(&mut self, id: &str, conversation: &Conversation) -> bool {
        let Some(session) = conversation.sessions.get(id) else {
            return false;
        };
        let Some((&row, &call)) = session
            .failures
            .range(self.selected.row.saturating_add(1)..)
            .next()
            .or_else(|| session.failures.first_key_value())
        else {
            return false;
        };
        let Row::Tools(turn) = session.rows[row] else {
            return false;
        };
        let tool = &session.turns[&turn].tools[call];
        self.groups.insert((id.into(), turn), true);
        self.tools.insert((id.into(), turn, tool.id.clone()), true);
        self.selected = Position {
            row,
            part: call * 4 + 1,
            ..Position::default()
        };
        self.top = Some(self.selected);
        self.focused = true;
        true
    }

    pub fn scroll(&mut self, id: &str, conversation: &Conversation, down: bool, page: bool) {
        let Some(session) = conversation.sessions.get(id) else {
            return;
        };
        let Some(mut pos) = self.painted.first().copied() else {
            return;
        };
        let count = if page {
            self.size.1.saturating_sub(1).max(1)
        } else {
            1
        };
        if !page {
            pos = self.selected;
        }
        for _ in 0..count {
            let Some(next) = self.step(id, conversation, session, pos, down) else {
                if down {
                    self.reset();
                } else {
                    self.home();
                }
                return;
            };
            pos = next;
        }
        pos = self
            .line(id, conversation, session, pos)
            .map_or(pos, |(pos, _)| pos);
        self.selected = pos;
        if page || !self.painted.contains(&pos) {
            self.top = Some(pos);
        } else if !down && self.top.is_none() {
            self.top = self.painted.first().copied();
        }
    }

    pub fn lines(
        &mut self,
        id: &str,
        conversation: &Conversation,
        width: u16,
        height: usize,
    ) -> Vec<Line<'static>> {
        let Some(session) = conversation.sessions.get(id) else {
            return Vec::new();
        };
        if width == 0 || height == 0 || session.rows.is_empty() {
            return Vec::new();
        }
        if self.generations.get(id) != Some(&session.generation)
            && self
                .generations
                .insert(id.into(), session.generation)
                .is_some()
        {
            self.reset();
        }
        // Keep the same row and text chunk across resize, even if wrapping changes.
        if self.size.0 != width {
            if let Some(top) = self.top.as_mut() {
                top.line = 0;
            }
            self.selected.line = 0;
        }
        self.size = (width, height);
        let forward = self.top.is_some();
        let mut pos = self.top.unwrap_or(Position {
            row: session.rows.len() - 1,
            part: usize::MAX,
            chunk: usize::MAX,
            line: usize::MAX,
        });
        pos.row = pos.row.min(session.rows.len() - 1);
        self.painted.clear();
        let mut lines = Vec::with_capacity(height);
        while lines.len() < height {
            if let Some((actual, line)) = self.line(id, conversation, session, pos) {
                self.painted.push(actual);
                lines.push(line);
                pos = actual;
            }
            let Some(next) = self.step(id, conversation, session, pos, forward) else {
                break;
            };
            pos = next;
        }
        if !forward {
            lines.reverse();
            self.painted.reverse();
        }
        if !self.painted.contains(&self.selected) {
            // The nearest actionable row is usually near the tail of a turn.
            self.selected = self
                .painted
                .iter()
                .rev()
                .find(|pos| matches!(session.rows[pos.row], Row::Tools(_) | Row::Child(_)))
                .or_else(|| self.painted.last())
                .copied()
                .unwrap_or_default();
        }
        if self.focused {
            for (pos, line) in self.painted.iter().zip(&mut lines) {
                if *pos == self.selected {
                    line.style = line.style.add_modifier(Modifier::REVERSED);
                }
            }
        }
        lines
    }

    fn group_open(&self, id: &str, turn: u64) -> bool {
        self.groups
            .get(&(id.into(), turn))
            .copied()
            .unwrap_or(!matches!(self.mode, Mode::Compact))
    }

    fn parts(&self, id: &str, session: &Session, row: usize) -> usize {
        match &session.rows[row] {
            Row::Text { .. } => 2,
            Row::Tools(turn) if self.group_open(id, *turn) => {
                1 + 4 * session.turns[turn].tools.len()
            }
            Row::Child(child) if self.children.contains(&(id.into(), child.clone())) => 2,
            _ => 1,
        }
    }

    fn piece(
        &self,
        id: &str,
        conversation: &Conversation,
        session: &Session,
        pos: Position,
    ) -> Option<Piece> {
        Some(match &session.rows[pos.row] {
            Row::Text { label, .. } if pos.part == 0 => Piece::new(
                label.to_string(),
                match *label {
                    "You" => Color::Cyan,
                    "Error" => Color::LightRed,
                    "Assistant" => Color::LightGreen,
                    _ => Color::DarkGray,
                },
            ),
            Row::Text { body, .. } => Piece {
                body: load_body(conversation, body),
                color: Color::Reset,
                markdown: true,
            },
            Row::Tools(turn) if pos.part == 0 => {
                let group = &session.turns[turn];
                let mark = if self.group_open(id, *turn) {
                    "▾"
                } else {
                    "▸"
                };
                let counts = group
                    .counts
                    .iter()
                    .take(6)
                    .map(|(name, count)| format!("{name} {count}"))
                    .collect::<Vec<_>>()
                    .join(" · ");
                Piece::new(
                    format!(
                        "{mark} Turn {turn} · {} tools · {} running · {} failed · {counts}",
                        group.tools.len(),
                        group.running,
                        group.failed
                    ),
                    if group.failed == 0 {
                        Color::Yellow
                    } else {
                        Color::LightRed
                    },
                )
            }
            Row::Tools(turn) => {
                let calls = &session.turns[turn].tools;
                let index = (pos.part - 1) / 4;
                let tool = &calls[index];
                let open = self
                    .tools
                    .get(&(id.into(), *turn, tool.id.clone()))
                    .copied()
                    .unwrap_or(matches!(self.mode, Mode::Verbose));
                match (pos.part - 1) % 4 {
                    0 => {
                        let status = if tool.failed {
                            "failed"
                        } else if tool.output.is_some() {
                            "done"
                        } else if tool.interrupted {
                            "interrupted"
                        } else {
                            "running"
                        };
                        let name = if tool.name.is_empty() {
                            "tool"
                        } else {
                            &tool.name
                        };
                        let args = excerpt(&load_body(conversation, &tool.arguments), 110);
                        let step = match tool.step {
                            Some(step)
                                if matches!(self.mode, Mode::Normal)
                                    && (index == 0 || calls[index - 1].step != Some(step)) =>
                            {
                                format!("Step {step}\n")
                            }
                            _ => String::new(),
                        };
                        Piece::new(
                            format!(
                                "{step}  {} {name} · {status} · {}  {args}",
                                if open { "▾" } else { "▸" },
                                tool.id
                            ),
                            if tool.failed {
                                Color::LightRed
                            } else {
                                Color::Yellow
                            },
                        )
                    }
                    1 if open && !tool.arguments.is_empty() => {
                        Piece::new(load_body(conversation, &tool.arguments), Color::DarkGray)
                    }
                    2 if open => {
                        Piece::new(load_body(conversation, tool.output.as_ref()?), Color::Reset)
                    }
                    2 if matches!(self.mode, Mode::Normal) => Piece::new(
                        excerpt(&load_body(conversation, tool.output.as_ref()?), 240),
                        Color::DarkGray,
                    ),
                    3 if open => Piece::new(
                        load_body(conversation, tool.error.as_ref()?),
                        Color::LightRed,
                    ),
                    _ => return None,
                }
            }
            Row::Child(child) => {
                let child_state = conversation.sessions.get(child)?;
                if pos.part == 0 {
                    let open = self.children.contains(&(id.into(), child.clone()));
                    Piece::new(
                        format!(
                            "{} Subagent · {child} · {}",
                            if open { "▾" } else { "▸" },
                            if child_state.running {
                                "running"
                            } else {
                                child_state.outcome.as_deref().unwrap_or("idle")
                            }
                        ),
                        Color::Magenta,
                    )
                } else {
                    let mut summary = format!(
                        "{} rows · {} tool turns · /view {child}",
                        child_state.rows.len(),
                        child_state.turns.len()
                    );
                    if let Some(body) = &child_state.summary {
                        summary.push('\n');
                        summary.push_str(&excerpt(&load_body(conversation, body), 500));
                    }
                    Piece::new(summary, Color::Gray)
                }
            }
        })
    }

    fn line(
        &mut self,
        id: &str,
        conversation: &Conversation,
        session: &Session,
        mut pos: Position,
    ) -> Option<(Position, Line<'static>)> {
        pos.part = pos.part.min(self.parts(id, session, pos.row) - 1);
        let piece = self.piece(id, conversation, session, pos)?;
        pos.chunk = pos
            .chunk
            .min(piece.body.len().saturating_sub(1) / CHUNK_BYTES);
        let lines = self.layout(&piece, pos.chunk);
        pos.line = pos.line.min(lines.len() - 1);
        Some((pos, lines[pos.line].clone().fg(piece.color)))
    }

    fn step(
        &mut self,
        id: &str,
        conversation: &Conversation,
        session: &Session,
        mut pos: Position,
        down: bool,
    ) -> Option<Position> {
        pos.part = pos.part.min(self.parts(id, session, pos.row) - 1);
        if let Some(piece) = self.piece(id, conversation, session, pos) {
            let last = piece.body.len().saturating_sub(1) / CHUNK_BYTES;
            pos.chunk = pos.chunk.min(last);
            let len = self.layout(&piece, pos.chunk).len();
            pos.line = pos.line.min(len - 1);
            if down && pos.line + 1 < len {
                pos.line += 1;
                return Some(pos);
            }
            if !down && pos.line > 0 {
                pos.line -= 1;
                return Some(pos);
            }
            if down && pos.chunk < last {
                pos.chunk += 1;
                pos.line = 0;
                return Some(pos);
            }
            if !down && pos.chunk > 0 {
                pos.chunk -= 1;
                pos.line = usize::MAX;
                return Some(pos);
            }
        }
        let edge = if down { 0 } else { usize::MAX };
        pos.chunk = edge;
        pos.line = edge;
        loop {
            if down && pos.part + 1 < self.parts(id, session, pos.row) {
                pos.part += 1;
            } else if !down && pos.part > 0 {
                pos.part -= 1;
            } else if down && pos.row + 1 < session.rows.len() {
                pos.row += 1;
                pos.part = 0;
            } else if !down && pos.row > 0 {
                pos.row -= 1;
                pos.part = self.parts(id, session, pos.row) - 1;
            } else {
                return None;
            }
            if self.piece(id, conversation, session, pos).is_some() {
                return Some(pos);
            }
        }
    }

    fn layout(&mut self, piece: &Piece, chunk: usize) -> &[Line<'static>] {
        let width = self.size.0.max(1);
        let body = Arc::downgrade(&piece.body);
        if let Some(index) = self.layouts.iter().position(|entry| {
            Weak::ptr_eq(&entry.body, &body)
                && entry.width == width
                && entry.markdown == piece.markdown
                && entry.chunk == chunk
        }) {
            return &self.layouts[index].lines;
        }
        let start = next_boundary(&piece.body, chunk * CHUNK_BYTES);
        let end = next_boundary(
            &piece.body,
            ((chunk + 1) * CHUNK_BYTES).min(piece.body.len()),
        );
        let in_fence = piece.markdown && start > 0 && self.in_fence(&piece.body, start);
        let lines = wrap(
            &piece.body[start..end],
            usize::from(width),
            piece.markdown,
            in_fence,
            start == 0 || piece.body.as_bytes()[start - 1] == b'\n',
        );
        let bytes = lines.capacity() * size_of::<Line<'static>>()
            + lines
                .iter()
                .map(|line| {
                    line.spans.capacity() * size_of::<Span<'static>>()
                        + line
                            .spans
                            .iter()
                            .map(|span| span.content.len())
                            .sum::<usize>()
                })
                .sum::<usize>();
        while self.layouts.len() >= CACHE_SIZE || self.layout_bytes + bytes > LAYOUT_BUDGET {
            let Some(old) = self.layouts.pop_front() else {
                break;
            };
            self.layout_bytes -= old.bytes;
        }
        self.layout_bytes += bytes;
        self.layouts.push_back(Layout {
            body,
            chunk,
            width,
            markdown: piece.markdown,
            lines,
            bytes,
        });
        // Just inserted, even when the ring was full.
        &self.layouts[self.layouts.len() - 1].lines
    }

    fn in_fence(&mut self, body: &Arc<String>, at: usize) -> bool {
        let source = Arc::downgrade(body);
        let fences = if let Some(index) = self
            .fences
            .iter()
            .position(|(text, _)| Weak::ptr_eq(text, &source))
        {
            &self.fences[index].1
        } else {
            let mut offset = 0;
            let mut fences = Vec::new();
            let mut odd = false;
            for line in body.split_inclusive('\n') {
                let trimmed = line.trim_start();
                if trimmed.starts_with("```") {
                    let index = offset + line.len() - trimmed.len();
                    odd ^= index < at;
                    if fences.len() < FENCE_BUDGET / size_of::<usize>() {
                        fences.push(index);
                    } else {
                        // Unusually dense fence sources are scanned without adding a cache entry.
                        offset += line.len();
                        for line in body[offset..].split_inclusive('\n') {
                            if offset >= at {
                                break;
                            }
                            let trimmed = line.trim_start();
                            odd ^= trimmed.starts_with("```")
                                && offset + line.len() - trimmed.len() < at;
                            offset += line.len();
                        }
                        return odd;
                    }
                }
                offset += line.len();
            }
            let bytes = fences.capacity() * size_of::<usize>();
            while self.fences.len() >= 16 || self.fence_bytes + bytes > FENCE_BUDGET {
                let Some((_, old)) = self.fences.pop_front() else {
                    break;
                };
                self.fence_bytes -= old.capacity() * size_of::<usize>();
            }
            self.fence_bytes += bytes;
            self.fences.push_back((source, fences));
            &self.fences[self.fences.len() - 1].1
        };
        fences.partition_point(|offset| *offset < at) % 2 != 0
    }
}

fn load_body(conversation: &Conversation, text: &dsh_state::conversation::Text) -> Arc<String> {
    conversation
        .body(text)
        .unwrap_or_else(|error| format!("Transcript read failed: {error}").into())
}

fn flip<T: Eq + std::hash::Hash>(set: &mut HashSet<T>, item: T) {
    if !set.remove(&item) {
        set.insert(item);
    }
}

fn next_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn excerpt(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = text[..end].split_whitespace().collect::<Vec<_>>().join(" ");
    if end < text.len() {
        output.push('…');
    }
    output
}

pub(super) fn safe_text(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\n' => '\n',
            '\t' => ' ',
            ch if ch.is_control() => '�',
            ch => ch,
        })
        .collect()
}

fn wrap(
    text: &str,
    width: usize,
    markdown: bool,
    mut code: bool,
    starts_line: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let start = index != 0 || starts_line;
        let raw = safe_text(raw);
        let line = raw.trim_start();
        let fence = markdown && start && line.starts_with("```");
        let style = if fence {
            code = !code;
            Style::default().fg(Color::DarkGray)
        } else if markdown && code {
            let trimmed = line.trim_start();
            Style::default().fg(if trimmed.starts_with("//") || trimmed.starts_with('#') {
                Color::DarkGray
            } else {
                Color::LightCyan
            })
        } else if markdown && start && line.starts_with('#') {
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD)
        } else if markdown && start && line.starts_with('>') {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default()
        };
        let table = markdown && start && !code && line.starts_with('|') && line.ends_with('|');
        let link = if markdown && !code && !fence {
            raw.find("](")
                .and_then(|mid| Some((raw[..mid].rfind('[')?, mid + 2 + raw[mid + 2..].find(')')?)))
        } else {
            None
        };
        let mut spans = Vec::new();
        let mut segment = String::new();
        let mut column = 0;
        let mut inline = false;
        let mut inline_style = style;
        for (index, grapheme) in raw.grapheme_indices(true) {
            let cells = grapheme.width();
            if column > 0 && column + cells > width {
                spans.push(Span::styled(std::mem::take(&mut segment), inline_style));
                lines.push(Line::from(std::mem::take(&mut spans)));
                column = 0;
            }
            if markdown && !code && grapheme == "`" && !fence {
                spans.push(Span::styled(std::mem::take(&mut segment), inline_style));
                inline = !inline;
                inline_style = if inline {
                    style.fg(Color::LightCyan)
                } else {
                    style
                };
            } else if link.is_some_and(|(start, end)| index == start || index == end + 1) && !inline
            {
                spans.push(Span::styled(std::mem::take(&mut segment), inline_style));
                inline_style = if link.is_some_and(|(start, _)| index == start) {
                    style
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::UNDERLINED)
                } else {
                    style
                };
            }
            if cells <= width {
                segment.push_str(if table && grapheme == "|" {
                    "│"
                } else {
                    grapheme
                });
                column += cells;
            }
        }
        spans.push(Span::styled(segment, inline_style));
        lines.push(Line::from(spans));
    }
    lines
}
