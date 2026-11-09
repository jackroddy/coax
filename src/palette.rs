use ratatui::style::Color;

/// Names: the program badge and the agent.
pub const REF: Color = Color::Cyan;

/// Work under way.
pub const WORKING: Color = Color::Yellow;

/// Work finished, and lines added.
pub const DONE: Color = Color::Green;

/// Failure, and lines removed.
pub const ALARM: Color = Color::Red;

/// The agent's own marks.
pub const AGENT: Color = Color::Magenta;

/// The focused pane's title.
pub const FOCUS: Color = Color::LightBlue;

/// Text on a filled badge.
pub const INK: Color = Color::Black;
