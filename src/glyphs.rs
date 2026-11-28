//! The characters coax draws its symbols with: the Unicode ones, or ASCII
//! stand-ins for a terminal whose font or locale lacks them.

use std::sync::OnceLock;

use ratatui::buffer::Buffer;
use serde::Deserialize;

/// Which characters the screen is drawn with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Glyphs {
    #[default]
    Unicode,
    Ascii,
}

/// The glyphs for a run: the config's `glyphs` when it names one, else ASCII
/// when the locale is not UTF-8 or the terminal is the Linux console.
pub fn detect(configured: Option<Glyphs>, env: impl Fn(&str) -> Option<String>) -> Glyphs {
    if let Some(glyphs) = configured {
        return glyphs;
    }
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|name| env(name).filter(|value| !value.is_empty()));
    let utf8 = locale.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("utf-8") || value.contains("utf8")
    });
    if !utf8 || env("TERM").as_deref() == Some("linux") {
        Glyphs::Ascii
    } else {
        Glyphs::Unicode
    }
}

static ACTIVE: OnceLock<Glyphs> = OnceLock::new();

/// Fix the glyphs for the run. Only the first call has an effect.
pub fn init(glyphs: Glyphs) {
    let _ = ACTIVE.set(glyphs);
}

/// The glyphs set by `init`, or Unicode when none were set.
pub fn get() -> Glyphs {
    *ACTIVE.get_or_init(Glyphs::default)
}

/// Rewrite each cell of a drawn frame that holds one of coax's symbols to
/// its ASCII stand-in, when the run draws in ASCII.
pub fn fold(buffer: &mut Buffer) {
    if get() == Glyphs::Ascii {
        to_ascii(buffer);
    }
}

/// Rewrite each cell that holds one of coax's symbols to its ASCII stand-in.
pub fn to_ascii(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        let mut chars = cell.symbol().chars();
        let (Some(ch), None) = (chars.next(), chars.next()) else {
            continue;
        };
        if let Some(stand_in) = stand_in(ch) {
            let mut text = [0; 4];
            cell.set_symbol(stand_in.encode_utf8(&mut text));
        }
    }
}

/// The one-column ASCII character drawn in place of `ch`, for the symbols
/// coax, ratatui's borders and cmr draw.
//
// every stand-in is one column, so nothing on the screen moves; text an
// agent sends that holds these characters is folded too
pub fn stand_in(ch: char) -> Option<char> {
    Some(match ch {
        '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' => {
            '+'
        }
        '─' | '┄' | '┈' => '-',
        '━' => '=',
        '│' | '┆' | '▌' | '▍' => '|',
        '┃' => '#',
        '≡' => '=',
        '◎' => '?',
        '±' => '+',
        '⌫' => 'x',
        '➜' | '›' | '▸' | '→' => '>',
        '❯' => '$',
        '↓' | '▾' => 'v',
        '↑' => '^',
        '∴' => ':',
        '◇' => '~',
        '•' | '▪' | '●' | '✻' => '*',
        '◆' => '+',
        '○' => 'o',
        '■' => '#',
        '□' => '_',
        '✓' => '+',
        '✗' => 'x',
        '⧉' => 'c',
        '⋯' | '…' | '·' => '.',
        '−' => '-',
        '◐' | '◴' | '⠋' | '⠼' | '⠇' => '|',
        '◓' | '◷' | '⠙' | '⠴' | '⠏' => '/',
        '◑' | '◶' | '⠹' | '⠦' => '-',
        '◒' | '◵' | '⠸' | '⠧' => '\\',
        '①'..='⑨' => char::from(b'1' + (ch as u32 - '①' as u32) as u8),
        '⑩'..='⑳' => char::from(b'A' + (ch as u32 - '⑩' as u32) as u8),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn a_utf8_locale_draws_unicode_and_anything_else_draws_ascii() {
        let utf8 = [("LANG", "en_US.UTF-8"), ("TERM", "xterm-ghostty")];
        assert_eq!(detect(None, env(&utf8)), Glyphs::Unicode);
        let lower = [("LANG", "en_US.utf8")];
        assert_eq!(detect(None, env(&lower)), Glyphs::Unicode);
        assert_eq!(detect(None, env(&[("LANG", "C")])), Glyphs::Ascii);
        assert_eq!(detect(None, env(&[])), Glyphs::Ascii);
    }

    #[test]
    fn lc_all_outranks_lc_ctype_which_outranks_lang() {
        let all = [("LC_ALL", "C"), ("LC_CTYPE", "en_US.UTF-8")];
        assert_eq!(detect(None, env(&all)), Glyphs::Ascii);
        let ctype = [("LC_CTYPE", "en_US.UTF-8"), ("LANG", "C")];
        assert_eq!(detect(None, env(&ctype)), Glyphs::Unicode);
        let empty = [("LC_ALL", ""), ("LANG", "en_US.UTF-8")];
        assert_eq!(detect(None, env(&empty)), Glyphs::Unicode);
    }

    #[test]
    fn the_linux_console_draws_ascii_even_in_a_utf8_locale() {
        let console = [("LANG", "en_US.UTF-8"), ("TERM", "linux")];
        assert_eq!(detect(None, env(&console)), Glyphs::Ascii);
    }

    #[test]
    fn the_config_outranks_the_environment() {
        let c = [("LANG", "C")];
        assert_eq!(detect(Some(Glyphs::Unicode), env(&c)), Glyphs::Unicode);
        let utf8 = [("LANG", "en_US.UTF-8")];
        assert_eq!(detect(Some(Glyphs::Ascii), env(&utf8)), Glyphs::Ascii);
    }

    #[test]
    fn chips_read_one_to_nine_then_letters() {
        assert_eq!(stand_in('①'), Some('1'));
        assert_eq!(stand_in('⑨'), Some('9'));
        assert_eq!(stand_in('⑩'), Some('A'));
        assert_eq!(stand_in('⑳'), Some('K'));
    }

    #[test]
    fn folding_leaves_plain_text_and_wide_characters_alone() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 1));
        buffer.set_string(0, 0, "╭─ a 日 ●", ratatui::style::Style::new());
        to_ascii(&mut buffer);
        let row: String = (0..12)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        assert_eq!(row, "+- a 日  *   ");
    }
}
