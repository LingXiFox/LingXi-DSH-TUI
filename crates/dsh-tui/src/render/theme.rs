//! Semantic terminal colors shared by the transcript, composer and overlays.

use ratatui::style::Color;

pub(super) const BACKGROUND: Color = Color::Rgb(17, 21, 26);
pub(super) const TEXT: Color = Color::Rgb(222, 228, 234);
pub(super) const MUTED: Color = Color::Rgb(148, 160, 173);
pub(super) const ACCENT: Color = Color::Rgb(119, 206, 193);
pub(super) const WARNING: Color = Color::Rgb(230, 187, 109);
pub(super) const ERROR: Color = Color::Rgb(242, 139, 143);
pub(super) const SURFACE: Color = Color::Rgb(31, 37, 45);
pub(super) const DIALOG: Color = Color::Rgb(27, 33, 40);
pub(super) const FIELD: Color = Color::Rgb(37, 44, 53);
pub(super) const SELECTION: Color = Color::Rgb(44, 69, 72);
pub(super) const BORDER: Color = Color::Rgb(74, 89, 100);
pub(super) const VEIL: Color = Color::Rgb(9, 12, 15);
pub(super) const VEILED_TEXT: Color = Color::Rgb(92, 101, 108);
