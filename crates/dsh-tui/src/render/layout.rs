//! Layout helpers that are easier to test on their own than through a frame.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// A rectangle centred inside `area`, clamped to the available space.
///
/// Used for modal overlays; on small terminals the popup shrinks instead of
/// drawing outside the frame.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .split(rows[1])[1]
}

/// Whether a terminal is large enough for the comfortable layout.
pub fn is_usable(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
}

/// Width below which the side panel crowds the activity log.
pub const MIN_WIDTH: u16 = 60;
/// Minimum height for the full layout.
pub const MIN_HEIGHT: u16 = 16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centers_within_larger_area() {
        let area = Rect::new(0, 0, 100, 40);
        let popup = centered(area, 40, 10);
        assert_eq!(popup.width, 40);
        assert_eq!(popup.height, 10);
        assert_eq!(popup.x, 30);
        assert_eq!(popup.y, 15);
    }

    #[test]
    fn clamps_to_area_on_small_terminals() {
        let area = Rect::new(0, 0, 30, 8);
        let popup = centered(area, 56, 14);
        assert_eq!(popup.width, 30);
        assert_eq!(popup.height, 8);
        assert_eq!(popup.x, 0);
        assert_eq!(popup.y, 0);
    }

    #[test]
    fn usability_threshold_is_explicit() {
        assert!(is_usable(Rect::new(0, 0, MIN_WIDTH, MIN_HEIGHT)));
        assert!(!is_usable(Rect::new(0, 0, MIN_WIDTH - 1, MIN_HEIGHT)));
        assert!(!is_usable(Rect::new(0, 0, MIN_WIDTH, MIN_HEIGHT - 1)));
    }
}
