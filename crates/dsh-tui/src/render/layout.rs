//! Layout helpers that are easier to test on their own than through a frame.

use ratatui::layout::Rect;

/// A rectangle centred inside `area`, clamped to the available space.
///
/// Used for modal overlays; on small terminals the popup shrinks instead of
/// drawing outside the frame.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

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
}
