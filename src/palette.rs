use std::str::FromStr;
use std::sync::OnceLock;

use ratatui::style::Color;
use serde::{Deserialize, Deserializer, de};

/// Text on a filled badge.
pub const INK: Color = Color::Black;

/// The colours the screen is drawn with, named as omarchy names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub accent: Color,

    /// Dim text and rules. `None` draws them with the terminal's dim attribute.
    pub muted: Option<Color>,
    pub selection: Color,
    pub foreground: Color,
    pub red: Color,
    pub yellow: Color,
    pub orange: Color,
    pub green: Color,
    pub cyan: Color,
    pub blue: Color,
    pub magenta: Color,
}

impl Palette {
    /// The colour called `name`, or `None` for a name outside the palette.
    pub fn named(&self, name: &str) -> Option<Color> {
        Some(match name {
            "accent" => self.accent,
            "muted" => self.muted.unwrap_or(Color::DarkGray),
            "selection" => self.selection,
            "foreground" => self.foreground,
            "red" => self.red,
            "yellow" => self.yellow,
            "orange" => self.orange,
            "green" => self.green,
            "cyan" => self.cyan,
            "blue" => self.blue,
            "magenta" => self.magenta,
            _ => return None,
        })
    }
}

/// The `[palette]` table of `config.toml`, before the fallbacks are filled in.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteConfig {
    #[serde(default, deserialize_with = "colour")]
    accent: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    muted: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    selection: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    foreground: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    red: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    yellow: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    orange: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    green: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    cyan: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    blue: Option<Color>,
    #[serde(default, deserialize_with = "colour")]
    magenta: Option<Color>,
}

fn colour<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Color>, D::Error> {
    let text = String::deserialize(deserializer)?;
    match Color::from_str(&text) {
        Ok(color) => Ok(Some(color)),
        Err(_) => Err(de::Error::custom(format!(
            "`{text}` is not a colour: use \"#RRGGBB\" or an ANSI colour name"
        ))),
    }
}

impl PaletteConfig {
    pub fn resolve(&self) -> Palette {
        Palette {
            accent: self.accent.unwrap_or(Color::Yellow),
            muted: self.muted,
            selection: self.selection.unwrap_or(Color::DarkGray),
            foreground: self.foreground.unwrap_or(Color::Reset),
            red: self.red.unwrap_or(Color::Red),
            yellow: self.yellow.unwrap_or(Color::Yellow),
            orange: self.orange.unwrap_or(Color::LightRed),
            green: self.green.unwrap_or(Color::Green),
            cyan: self.cyan.unwrap_or(Color::Cyan),
            blue: self.blue.unwrap_or(Color::Blue),
            magenta: self.magenta.unwrap_or(Color::Magenta),
        }
    }
}

impl Default for Palette {
    fn default() -> Palette {
        PaletteConfig::default().resolve()
    }
}

static ACTIVE: OnceLock<Palette> = OnceLock::new();

/// Fix the palette for the run. Only the first call has an effect.
pub fn init(palette: Palette) {
    let _ = ACTIVE.set(palette);
}

/// The palette set by `init`, or the ANSI fallbacks when none was set.
pub fn get() -> &'static Palette {
    ACTIVE.get_or_init(Palette::default)
}
