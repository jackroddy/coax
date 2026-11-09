//! Cursor movement over the prompt's text. A cursor is a byte index on a char
//! boundary, and a line is the text between newlines.

use unicode_width::UnicodeWidthChar;

pub fn char_before(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

pub fn char_after(text: &str, at: usize) -> usize {
    text[at..]
        .chars()
        .next()
        .map_or(at, |ch| at + ch.len_utf8())
}

/// The start of the word before `at`. A word is a run of non-whitespace.
pub fn word_before(text: &str, at: usize) -> usize {
    let head = text[..at].trim_end();
    head.rfind(char::is_whitespace).map_or(0, |i| {
        i + text[i..].chars().next().map_or(1, char::len_utf8)
    })
}

/// The end of the word after `at`.
pub fn word_after(text: &str, at: usize) -> usize {
    let tail = &text[at..];
    let word_start = tail.len() - tail.trim_start().len();
    let word = &tail[word_start..];
    let word_len = word.find(char::is_whitespace).unwrap_or(word.len());
    at + word_start + word_len
}

pub fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

pub fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}

/// The same display column one line up, or `None` on the first line.
pub fn line_above(text: &str, at: usize) -> Option<usize> {
    let start = line_start(text, at);
    if start == 0 {
        return None;
    }
    let above = line_start(text, start - 1);
    Some(at_column(&text[above..start - 1], column(&text[start..at])) + above)
}

/// The same display column one line down, or `None` on the last line.
pub fn line_below(text: &str, at: usize) -> Option<usize> {
    let end = line_end(text, at);
    if end == text.len() {
        return None;
    }
    let below = end + 1;
    let below_end = line_end(text, below);
    let from = line_start(text, at);
    Some(at_column(&text[below..below_end], column(&text[from..at])) + below)
}

fn column(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(0)).sum()
}

/// The byte index in `line` at or before display column `wanted`.
fn at_column(line: &str, wanted: usize) -> usize {
    let mut used = 0;
    for (i, ch) in line.char_indices() {
        let width = ch.width().unwrap_or(0);
        if used + width > wanted {
            return i;
        }
        used += width;
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_over_a_multibyte_char_lands_on_a_boundary() {
        let text = "aé日😀b";
        assert_eq!(char_after(text, 0), 1);
        assert_eq!(char_after(text, 1), 3);
        assert_eq!(char_after(text, 3), 6);
        assert_eq!(char_after(text, 6), 10);
        assert_eq!(char_before(text, 10), 6);
        assert_eq!(char_before(text, 6), 3);
        assert_eq!(char_before(text, 0), 0);
        assert_eq!(char_after(text, text.len()), text.len());
    }

    #[test]
    fn a_word_jump_skips_the_spaces_and_then_the_word() {
        let text = "one  two three";
        assert_eq!(word_before(text, 14), 9);
        assert_eq!(word_before(text, 9), 5);
        assert_eq!(word_before(text, 7), 5);
        assert_eq!(word_before(text, 3), 0);
        assert_eq!(word_after(text, 0), 3);
        assert_eq!(word_after(text, 3), 8);
        assert_eq!(word_after(text, 14), 14);
    }

    #[test]
    fn a_word_jump_works_over_multibyte_words() {
        let text = "日本 語é";
        assert_eq!(word_after(text, 0), 6);
        assert_eq!(word_before(text, text.len()), 7);
        assert_eq!(word_before(text, 6), 0);
    }

    #[test]
    fn a_line_runs_between_newlines() {
        let text = "ab\ncde\n";
        assert_eq!(line_start(text, 4), 3);
        assert_eq!(line_end(text, 4), 6);
        assert_eq!(line_start(text, 1), 0);
        assert_eq!(line_end(text, 1), 2);
        assert_eq!(line_start(text, 7), 7);
        assert_eq!(line_end(text, 7), 7);
    }

    #[test]
    fn moving_between_lines_keeps_the_display_column() {
        let text = "日本語\nabcdef\nx";
        assert_eq!(line_above(text, 0), None);
        assert_eq!(line_below(text, text.len()), None);
        // after 日本 (column 4) goes to column 4 below
        assert_eq!(line_below(text, 6), Some(10 + 4));
        // column 3 falls inside 本, so the cursor lands before it
        assert_eq!(line_above(text, 10 + 3), Some(3));
        // a short line clamps to its end
        assert_eq!(line_below(text, 10 + 5), Some(text.len()));
        assert_eq!(line_above(text, text.len()), Some(10 + 1));
    }
}
