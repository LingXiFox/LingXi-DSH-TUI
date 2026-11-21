//! Grapheme-aware editing of prompt and interaction drafts.

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Text and selection, addressed by UTF-8 byte offsets at grapheme boundaries.
#[derive(Debug, Default)]
pub struct Draft {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

/// Cursor movements independent of terminal key codes.
#[derive(Debug, Clone, Copy)]
pub enum Motion {
    /// Previous grapheme.
    Left,
    /// Next grapheme.
    Right,
    /// Previous logical line.
    Up,
    /// Next logical line.
    Down,
    /// Start of this line.
    Home,
    /// End of this line.
    End,
}

impl Draft {
    /// The full draft.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Current insertion point.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Selected UTF-8 byte range.
    pub fn selection(&self) -> Range<usize> {
        let anchor = self.anchor.unwrap_or(self.cursor);
        self.cursor.min(anchor)..self.cursor.max(anchor)
    }

    /// Select the whole draft.
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Insert typed or pasted text in place of the current selection.
    pub fn insert(&mut self, text: &str) {
        let range = self.selection();
        let start = range.start;
        self.text.replace_range(range, text);
        self.cursor = start + text.len();
        self.repair_cursor();
        self.anchor = None;
    }

    /// Replace this draft with a recovered submission.
    pub fn replace(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.anchor = None;
    }

    /// Move the cursor, optionally extending the selection.
    pub fn move_to(&mut self, motion: Motion, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            let range = self.selection();
            self.anchor = None;
            if !range.is_empty() {
                match motion {
                    Motion::Left => {
                        self.cursor = range.start;
                        return;
                    }
                    Motion::Right => {
                        self.cursor = range.end;
                        return;
                    }
                    _ => {}
                }
            }
        }
        let start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
        self.cursor = match motion {
            Motion::Left => self.previous(),
            Motion::Right => self.next(),
            Motion::Home => start,
            Motion::End => end,
            Motion::Up if start > 0 => {
                let previous_end = start - 1;
                let previous_start = self.text[..previous_end].rfind('\n').map_or(0, |i| i + 1);
                self.line_column(start, previous_start, previous_end)
            }
            Motion::Down if end < self.text.len() => {
                let next = end + 1;
                let next_end = self.text[next..]
                    .find('\n')
                    .map_or(self.text.len(), |i| next + i);
                self.line_column(start, next, next_end)
            }
            _ => self.cursor,
        };
    }

    /// Delete the selection or one adjacent grapheme.
    pub fn delete(&mut self, backwards: bool) {
        let mut range = self.selection();
        if range.is_empty() {
            if backwards {
                range.start = self.previous();
            } else {
                range.end = self.next();
            }
        }
        self.cursor = range.start;
        self.text.replace_range(range, "");
        self.repair_cursor();
        self.anchor = None;
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next(&self) -> usize {
        self.cursor
            + self.text[self.cursor..]
                .graphemes(true)
                .next()
                .map_or(0, str::len)
    }

    fn line_column(&self, start: usize, target: usize, end: usize) -> usize {
        let column = self.text[start..self.cursor].graphemes(true).count();
        self.text[target..end]
            .grapheme_indices(true)
            .nth(column)
            .map_or(end, |(index, _)| target + index)
    }

    fn repair_cursor(&mut self) {
        // Insertion can join characters on either side into one grapheme.
        if self.cursor != self.text.len() {
            self.cursor = self
                .text
                .grapheme_indices(true)
                .find(|(i, _)| *i >= self.cursor)
                .map_or(self.text.len(), |(i, _)| i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_whole_cjk_and_combining_graphemes() {
        let mut draft = Draft::default();
        draft.insert("中文\ne\u{301}👩‍💻");
        draft.delete(true);
        assert_eq!(draft.text(), "中文\ne\u{301}");
        draft.move_to(Motion::Up, false);
        draft.move_to(Motion::Left, true);
        draft.insert("日");
        assert_eq!(draft.text(), "日文\ne\u{301}");
        draft.select_all();
        draft.delete(false);
        assert_eq!(draft.text(), "");
    }
}
