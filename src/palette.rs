use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Style};
use serde::{Deserialize, Deserializer, de};

/// The colours the screen is drawn with: the terminal's slots, named as
/// terminal themes name them, and coax's own roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
    pub foreground: Color,

    /// The colour painted on every cell, or `None` for the terminal's own.
    pub background: Option<Color>,

    /// Your messages, and whatever is under the pointer.
    pub accent: Color,

    /// Secondary text and rules.
    pub muted: Color,

    /// The background of a chosen row.
    pub selection: Color,
    pub orange: Color,

    /// A fill just off the background, for hover shading.
    pub tint: Color,
}

const TERMINAL_SLOTS: [&str; 18] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
    "foreground",
    "background",
];

/// coax's own slots and the terminal slot each takes when the config leaves
/// it out. `tint` is missing because its default is mixed.
const COAX_DEFAULTS: [(&str, &str); 4] = [
    ("accent", "yellow"),
    ("muted", "bright_black"),
    ("selection", "bright_black"),
    ("orange", "bright_red"),
];

/// The share of the foreground in `tint`, in percent.
const TINT_PERCENT: f32 = 7.0;

impl Palette {
    /// The terminal's own colours, before the coax slots are filled in.
    fn terminal() -> Palette {
        Palette {
            black: Color::Black,
            red: Color::Red,
            green: Color::Green,
            yellow: Color::Yellow,
            blue: Color::Blue,
            magenta: Color::Magenta,
            cyan: Color::Cyan,
            white: Color::Gray,
            bright_black: Color::DarkGray,
            bright_red: Color::LightRed,
            bright_green: Color::LightGreen,
            bright_yellow: Color::LightYellow,
            bright_blue: Color::LightBlue,
            bright_magenta: Color::LightMagenta,
            bright_cyan: Color::LightCyan,
            bright_white: Color::White,
            foreground: Color::Reset,
            background: None,
            accent: Color::Yellow,
            muted: Color::DarkGray,
            selection: Color::DarkGray,
            orange: Color::LightRed,
            tint: Color::DarkGray,
        }
    }

    /// The colour called `name`, or `None` for a name outside the palette.
    pub fn named(&self, name: &str) -> Option<Color> {
        Some(match name {
            "black" => self.black,
            "red" => self.red,
            "green" => self.green,
            "yellow" => self.yellow,
            "blue" => self.blue,
            "magenta" => self.magenta,
            "cyan" => self.cyan,
            "white" => self.white,
            "bright_black" => self.bright_black,
            "bright_red" => self.bright_red,
            "bright_green" => self.bright_green,
            "bright_yellow" => self.bright_yellow,
            "bright_blue" => self.bright_blue,
            "bright_magenta" => self.bright_magenta,
            "bright_cyan" => self.bright_cyan,
            "bright_white" => self.bright_white,
            "foreground" => self.foreground,
            "background" => self.background.unwrap_or(Color::Reset),
            "accent" => self.accent,
            "muted" => self.muted,
            "selection" => self.selection,
            "orange" => self.orange,
            "tint" => self.tint,
            _ => return None,
        })
    }

    fn set(&mut self, name: &str, color: Color) {
        let slot = match name {
            "black" => &mut self.black,
            "red" => &mut self.red,
            "green" => &mut self.green,
            "yellow" => &mut self.yellow,
            "blue" => &mut self.blue,
            "magenta" => &mut self.magenta,
            "cyan" => &mut self.cyan,
            "white" => &mut self.white,
            "bright_black" => &mut self.bright_black,
            "bright_red" => &mut self.bright_red,
            "bright_green" => &mut self.bright_green,
            "bright_yellow" => &mut self.bright_yellow,
            "bright_blue" => &mut self.bright_blue,
            "bright_magenta" => &mut self.bright_magenta,
            "bright_cyan" => &mut self.bright_cyan,
            "bright_white" => &mut self.bright_white,
            "foreground" => &mut self.foreground,
            "background" => {
                self.background = Some(color);
                return;
            }
            "accent" => &mut self.accent,
            "muted" => &mut self.muted,
            "selection" => &mut self.selection,
            "orange" => &mut self.orange,
            "tint" => &mut self.tint,
            _ => unreachable!("`{name}` is checked when the config is read"),
        };
        *slot = color;
    }
}

/// The foreground and background the terminal reported, as RGB.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reported {
    pub foreground: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
}

/// The `[palette]` table of `config.toml`, before the defaults are filled in.
#[derive(Debug, Default)]
pub struct PaletteConfig {
    values: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value {
    Colour(Color),

    /// A terminal slot's name. Only a coax slot takes one.
    Slot(&'static str),
}

impl<'de> Deserialize<'de> for PaletteConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, String>::deserialize(deserializer)?;
        let mut values = BTreeMap::new();
        for (name, text) in raw {
            let value = parse_value(&name, &text).map_err(de::Error::custom)?;
            values.insert(name, value);
        }
        Ok(PaletteConfig { values })
    }
}

fn parse_value(name: &str, text: &str) -> Result<Value, String> {
    let coax = name == "tint" || COAX_DEFAULTS.iter().any(|(slot, _)| *slot == name);
    if !coax && !TERMINAL_SLOTS.contains(&name) {
        return Err(format!("`{name}` is not a palette slot"));
    }
    if coax && let Some(slot) = TERMINAL_SLOTS.iter().find(|slot| **slot == text) {
        return Ok(Value::Slot(slot));
    }
    Color::from_str(text).map(Value::Colour).map_err(|_| {
        format!(
            "`{text}` is not a colour: use \"#RRGGBB\", an ANSI colour name, or for \
             a coax slot the name of a terminal slot"
        )
    })
}

impl PaletteConfig {
    /// The palette, with the colours the terminal `reported` as the source of
    /// `tint` when the config gives no RGB of its own.
    pub fn resolve(&self, reported: Reported) -> Palette {
        let mut palette = Palette::terminal();
        for name in TERMINAL_SLOTS {
            if let Some(Value::Colour(color)) = self.values.get(name) {
                palette.set(name, *color);
            }
        }
        for (name, default) in COAX_DEFAULTS {
            let color = self.coax_value(name, &palette).or(palette.named(default));
            palette.set(name, color.unwrap_or(Color::Reset));
        }
        let mixed = rgb(palette.background)
            .or(reported.background)
            .zip(rgb(Some(palette.foreground)).or(reported.foreground))
            .map(|(background, foreground)| mix(background, foreground));
        palette.tint = self
            .coax_value("tint", &palette)
            .or(mixed)
            .unwrap_or(palette.selection);
        palette
    }

    fn coax_value(&self, name: &str, palette: &Palette) -> Option<Color> {
        match self.values.get(name)? {
            Value::Colour(color) => Some(*color),
            Value::Slot(slot) => palette.named(slot),
        }
    }
}

fn rgb(color: Option<Color>) -> Option<[u8; 3]> {
    match color? {
        Color::Rgb(r, g, b) => Some([r, g, b]),
        _ => None,
    }
}

fn mix(background: [u8; 3], foreground: [u8; 3]) -> Color {
    let channel = |i: usize| {
        let (b, f) = (background[i] as f32, foreground[i] as f32);
        (b + (f - b) * TINT_PERCENT / 100.0).round() as u8
    };
    Color::Rgb(channel(0), channel(1), channel(2))
}

impl Default for Palette {
    fn default() -> Palette {
        PaletteConfig::default().resolve(Reported::default())
    }
}

/// How long the terminal has to answer the colour queries.
const QUERY_WAIT: Duration = Duration::from_millis(100);

/// Ask the terminal for its foreground and background with OSC 10 and OSC
/// 11. Off a terminal, or with no answer in `QUERY_WAIT`, both are `None`.
pub fn query_terminal() -> Reported {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Reported::default();
    }
    if crossterm::terminal::enable_raw_mode().is_err() {
        return Reported::default();
    }

    // every terminal answers the DA1 query at the end, so
    // its reply marks that the colour replies are all in
    let mut stdout = io::stdout();
    let _ = stdout.write_all(b"\x1b]10;?\x07\x1b]11;?\x07\x1b[c");
    let _ = stdout.flush();
    let mut replies = Vec::new();
    let deadline = Instant::now() + QUERY_WAIT;
    while !ends_with_device_attributes(&replies) {
        let left = deadline.saturating_duration_since(Instant::now());
        let Some(chunk) = read_stdin(left) else {
            break;
        };
        replies.extend_from_slice(&chunk);
    }
    let _ = crossterm::terminal::disable_raw_mode();
    parse_replies(&replies)
}

/// The bytes waiting on stdin within `wait`, or `None` when there are none.
fn read_stdin(wait: Duration) -> Option<Vec<u8>> {
    let mut poll = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = wait.as_millis().min(i32::MAX as u128) as i32;
    if millis == 0 {
        return None;
    }

    // SAFETY: `poll` is one valid pollfd, and the count passed is 1
    if unsafe { libc::poll(&mut poll, 1, millis) } <= 0 {
        return None;
    }
    let mut buffer = [0u8; 1024];

    // SAFETY: `buffer` is writable for the length passed
    let read = unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
    (read > 0).then(|| buffer[..read as usize].to_vec())
}

fn ends_with_device_attributes(replies: &[u8]) -> bool {
    replies
        .windows(3)
        .rposition(|window| window == b"\x1b[?")
        .is_some_and(|at| replies[at..].contains(&b'c'))
}

/// The colours in OSC 10 and OSC 11 replies, as in
/// `\x1b]11;rgb:1f1f/1a1a/2424\x07`, ended by BEL or by ESC `\`.
fn parse_replies(replies: &[u8]) -> Reported {
    let text = String::from_utf8_lossy(replies);
    let colour = |code: &str| {
        let start = text.find(&format!("\x1b]{code};rgb:"))? + code.len() + 7;
        let rest = &text[start..];
        let end = rest.find(['\x07', '\x1b'])?;
        let mut channels = rest[..end].split('/').map(channel);
        Some([channels.next()??, channels.next()??, channels.next()??])
    };
    Reported {
        foreground: colour("10"),
        background: colour("11"),
    }
}

/// One channel of an `rgb:` reply, from 1 to 4 hex digits, scaled to 8 bits.
fn channel(hex: &str) -> Option<u8> {
    if hex.is_empty() || hex.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let max = (1u32 << (4 * hex.len())) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

/// The style every cell starts from: the foreground, and the background when
/// one is set.
pub fn base() -> Style {
    let palette = get();
    let style = Style::new().fg(palette.foreground);
    match palette.background {
        Some(background) => style.bg(background),
        None => style,
    }
}

static ACTIVE: OnceLock<Palette> = OnceLock::new();

/// Fix the palette for the run. Only the first call has an effect.
pub fn init(palette: Palette) {
    let _ = ACTIVE.set(palette);
}

/// The palette set by `init`, or the terminal's colours when none was set.
pub fn get() -> &'static Palette {
    ACTIVE.get_or_init(Palette::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Result<PaletteConfig, toml::de::Error> {
        toml::from_str(text)
    }

    const JACKS_TERMINAL: Reported = Reported {
        foreground: Some([0xe7, 0xdd, 0xc8]),
        background: Some([0x1f, 0x1a, 0x24]),
    };

    #[test]
    fn the_coax_slots_default_to_their_terminal_slots() {
        let palette = config("bright_black = \"#6d6977\"\nyellow = \"#da915c\"\n")
            .unwrap()
            .resolve(Reported::default());
        assert_eq!(palette.accent, Color::Rgb(0xda, 0x91, 0x5c));
        assert_eq!(palette.muted, Color::Rgb(0x6d, 0x69, 0x77));
        assert_eq!(palette.selection, Color::Rgb(0x6d, 0x69, 0x77));
        assert_eq!(palette.orange, Color::LightRed);
    }

    #[test]
    fn a_coax_slot_named_after_a_terminal_slot_follows_it() {
        let palette = config("accent = \"bright_cyan\"\nbright_cyan = \"#112233\"\n")
            .unwrap()
            .resolve(Reported::default());
        assert_eq!(palette.accent, Color::Rgb(0x11, 0x22, 0x33));
    }

    #[test]
    fn a_terminal_slot_takes_a_colour_and_not_another_slot() {
        assert!(config("red = \"accent\"\n").is_err());
        assert!(config("red = \"tint\"\n").is_err());
        let palette = config("red = \"#c26a7a\"\n")
            .unwrap()
            .resolve(Reported::default());
        assert_eq!(palette.red, Color::Rgb(0xc2, 0x6a, 0x7a));
    }

    #[test]
    fn the_tint_mixes_seven_percent_of_the_foreground_into_the_background() {
        let palette = PaletteConfig::default().resolve(JACKS_TERMINAL);
        assert_eq!(palette.tint, Color::Rgb(0x2d, 0x28, 0x2f));
    }

    #[test]
    fn a_configured_background_and_foreground_win_over_the_reported_ones() {
        let palette = config("background = \"#000000\"\nforeground = \"#646464\"\n")
            .unwrap()
            .resolve(JACKS_TERMINAL);
        assert_eq!(palette.background, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(palette.tint, Color::Rgb(7, 7, 7));
    }

    #[test]
    fn without_both_colours_as_rgb_the_tint_is_the_selection() {
        let palette = config("selection = \"#37344a\"\n")
            .unwrap()
            .resolve(Reported::default());
        assert_eq!(palette.tint, Color::Rgb(0x37, 0x34, 0x4a));
        assert_eq!(palette.background, None);
    }

    #[test]
    fn every_slot_has_a_name_for_color_links() {
        let palette = Palette::default();
        for name in TERMINAL_SLOTS {
            assert!(palette.named(name).is_some(), "{name}");
        }
        for name in ["accent", "muted", "selection", "orange", "tint"] {
            assert!(palette.named(name).is_some(), "{name}");
        }
        assert_eq!(palette.named("brown"), None);
    }

    #[test]
    fn colour_replies_parse_with_either_terminator_and_any_digit_count() {
        let replies = b"\x1b]10;rgb:e7e7/dddd/c8c8\x07\x1b]11;rgb:1f/1a/24\x1b\\\x1b[?62;22c";
        let reported = parse_replies(replies);
        assert_eq!(reported, JACKS_TERMINAL);
        assert!(ends_with_device_attributes(replies));
        assert_eq!(parse_replies(b"\x1b[?62c"), Reported::default());
    }
}
