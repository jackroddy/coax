//! The text field every editable box uses: the prompt and a form's text
//! fields. A cursor is a byte index on a char boundary, a line is the text
//! between newlines, and a row is the part of a line drawn on one screen row.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Editable text and the cursor in it.
#[derive(Default)]
pub struct TextField {
    pub text: String,

    /// The cursor's byte index into `text`, always on a char boundary.
    pub cursor: usize,
}

/// What a key did to a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keyed {
    Unused,
    Moved,
    Edited,
}

/// The rows of a field shown at a width and a most-rows limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// Each shown row's byte range in the text.
    pub rows: Vec<Range<usize>>,

    /// Whether `rows` begins at the start of the text.
    pub starts_here: bool,

    /// Column and row of the cursor among `rows`.
    pub cursor: (usize, usize),
}

impl TextField {
    /// A field holding `text`, with the cursor at its end.
    pub fn new(text: String) -> TextField {
        TextField {
            cursor: text.len(),
            text,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// The text, leaving the field empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Insert `text` at the cursor and move the cursor past it.
    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// Remove the text from `from` to the cursor, or from the cursor to
    /// `from` when that is later.
    fn remove_between(&mut self, from: usize) {
        let (start, end) = (from.min(self.cursor), from.max(self.cursor));
        self.text.replace_range(start..end, "");
        self.cursor = start;
    }

    /// Handle an editing key, with the text drawn `width` columns wide.
    pub fn key(&mut self, key: &KeyEvent, width: usize) -> Keyed {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let text = &self.text;
        let at = self.cursor;
        let moved = match key.code {
            KeyCode::Up => row_above(text, at, width),
            KeyCode::Down => row_below(text, at, width),
            KeyCode::Left if alt || ctrl => Some(word_before(text, at)),
            KeyCode::Char('b') if alt => Some(word_before(text, at)),
            KeyCode::Right if alt || ctrl => Some(word_after(text, at)),
            KeyCode::Char('f') if alt => Some(word_after(text, at)),
            KeyCode::Left => Some(char_before(text, at)),
            KeyCode::Right => Some(char_after(text, at)),
            KeyCode::Char('a') if ctrl => Some(line_start(text, at)),
            KeyCode::Char('e') if ctrl => Some(line_end(text, at)),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(text.len()),
            _ => None,
        };
        if let Some(at) = moved {
            self.cursor = at;
            return Keyed::Moved;
        }
        match key.code {
            KeyCode::Enter if alt || shift => self.insert("\n"),
            KeyCode::Backspace => self.remove_between(char_before(text, at)),
            KeyCode::Delete => self.remove_between(char_after(text, at)),
            KeyCode::Char('w') if ctrl => self.remove_between(word_before(text, at)),
            KeyCode::Char('k') if ctrl => {
                let mut end = line_end(text, at);
                if end == at {
                    end = char_after(text, end);
                }
                self.remove_between(end);
            }
            KeyCode::Char('u') if ctrl => self.clear(),
            KeyCode::Char(ch) if !ctrl => self.insert(ch.encode_utf8(&mut [0; 4])),
            _ => return Keyed::Unused,
        }
        Keyed::Edited
    }

    /// The byte index a click lands on at `column` of row `row` of `view`.
    /// A click past a row's end lands at that end, and one below the last
    /// row at the end of the text.
    pub fn at(&self, view: &View, column: usize, row: usize) -> usize {
        let Some(range) = view.rows.get(row) else {
            return self.text.len();
        };
        let Some(shown) = self.text.get(range.clone()) else {
            return self.text.len();
        };
        let at = range.start + at_column(shown, column);

        // the end of a row that wraps is the start of the
        // next row, where the cursor would be drawn
        let wraps = self.text[at..].starts_with(|ch| ch != '\n');
        if at == range.end && wraps {
            return char_before(&self.text, at);
        }
        at
    }

    /// The rows shown at `width` columns and at most `max_rows` rows, a
    /// window that keeps the cursor's row in view.
    pub fn view(&self, width: usize, max_rows: usize) -> View {
        let all = wrap_rows(&self.text, width);
        let shown = max_rows.max(1);
        let cursor = self.cursor.min(self.text.len());
        let cursor_row = cursor_row(&all, cursor);
        let skipped = all.len().saturating_sub(shown).min(cursor_row);
        let end = (skipped + shown).min(all.len());
        let column = self.text[all[cursor_row].start..cursor].width();
        View {
            rows: all[skipped..end].to_vec(),
            starts_here: skipped == 0,
            cursor: (column.min(width.max(1) - 1), cursor_row - skipped),
        }
    }
}

fn char_before(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn char_after(text: &str, at: usize) -> usize {
    text[at..]
        .chars()
        .next()
        .map_or(at, |ch| at + ch.len_utf8())
}

/// The start of the word before `at`. A word is a run of non-whitespace.
fn word_before(text: &str, at: usize) -> usize {
    let head = text[..at].trim_end();
    head.rfind(char::is_whitespace).map_or(0, |i| {
        i + text[i..].chars().next().map_or(1, char::len_utf8)
    })
}

/// The end of the word after `at`.
fn word_after(text: &str, at: usize) -> usize {
    let tail = &text[at..];
    let word_start = tail.len() - tail.trim_start().len();
    let word = &tail[word_start..];
    let word_len = word.find(char::is_whitespace).unwrap_or(word.len());
    at + word_start + word_len
}

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}

/// The same display column one drawn row up, or `None` on the first row.
fn row_above(text: &str, at: usize, width: usize) -> Option<usize> {
    let rows = wrap_rows(text, width);
    let row = cursor_row(&rows, at);
    Some(same_column(text, &rows, row, row.checked_sub(1)?, at))
}

/// The same display column one drawn row down, or `None` on the last row.
fn row_below(text: &str, at: usize, width: usize) -> Option<usize> {
    let rows = wrap_rows(text, width);
    let row = cursor_row(&rows, at);
    if row + 1 == rows.len() {
        return None;
    }
    Some(same_column(text, &rows, row, row + 1, at))
}

/// The row of `rows` the cursor at `at` is drawn on. Where two rows of one
/// line meet, the cursor is drawn at the start of the second.
fn cursor_row(rows: &[Range<usize>], at: usize) -> usize {
    rows.iter().rposition(|row| row.start <= at).unwrap_or(0)
}

/// The byte index on row `to` at the display column `at` has on row `from`.
fn same_column(text: &str, rows: &[Range<usize>], from: usize, to: usize, at: usize) -> usize {
    let wanted = column(&text[rows[from].start..at]);
    let landed = rows[to].start + at_column(&text[rows[to].clone()], wanted);

    // the end of a row that wraps is the start of the
    // next row, where the cursor would be drawn
    if rows.get(to + 1).is_some_and(|next| next.start == landed) {
        return char_before(text, landed);
    }
    landed
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

/// Break `text` into rows of at most `width` characters at word breaks, as
/// byte ranges of `text`. The space a row breaks at stays at its end, one
/// column past `width` if need be. Each line, ended by a newline, starts a new
/// row. A word longer than a row is split, and the last row is never full, so
/// the cursor always has a column after the last character.
fn wrap_rows(text: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        wrap_line(line, offset, width, &mut rows);
        offset += line.len() + 1;
    }
    if rows
        .last()
        .is_some_and(|row| text[row.clone()].width() >= width)
    {
        rows.push(text.len()..text.len());
    }
    rows
}

/// Push the rows of one line, which starts at byte `offset` of the text.
fn wrap_line(line: &str, offset: usize, width: usize, rows: &mut Vec<Range<usize>>) {
    let chars: Vec<(usize, char, usize)> = line
        .char_indices()
        .map(|(i, ch)| (i, ch, ch.width().unwrap_or(0)))
        .collect();
    let mut start = 0;
    let mut used = 0;
    for token in chars.chunk_by(|a, b| (a.1 == ' ') == (b.1 == ' ')) {
        let is_space = token[0].1 == ' ';
        let token_width: usize = token.iter().map(|&(_, _, w)| w).sum();
        if used + token_width > width && !is_space && used > 0 {
            rows.push(offset + start..offset + token[0].0);
            start = token[0].0;
            used = 0;
        }
        let room = if is_space { width + 1 } else { width };
        for &(i, _, w) in token {
            if used + w > room && used > 0 {
                rows.push(offset + start..offset + i);
                start = i;
                used = 0;
            }
            used += w;
        }
    }
    rows.push(offset + start..offset + line.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(field: &mut TextField, code: KeyCode, modifiers: KeyModifiers) -> Keyed {
        field.key(&KeyEvent::new(code, modifiers), 80)
    }

    #[test]
    fn typed_characters_go_in_at_the_cursor() {
        let mut field = TextField::new("ac".to_string());
        press(&mut field, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(
            press(&mut field, KeyCode::Char('b'), KeyModifiers::NONE),
            Keyed::Edited
        );
        assert_eq!(field.text, "abc");
        assert_eq!(field.cursor, 2);
        press(&mut field, KeyCode::Enter, KeyModifiers::SHIFT);
        press(&mut field, KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(field.text, "ab\n\nc");
    }

    #[test]
    fn the_deleting_keys_remove_a_char_a_word_or_to_the_end_of_the_line() {
        let mut field = TextField::new("one two\nthree".to_string());
        press(&mut field, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(field.text, "one two\nthre");
        press(&mut field, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(field.text, "one two\n");
        press(&mut field, KeyCode::Home, KeyModifiers::NONE);
        press(&mut field, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(field.text, "ne two\n");
        press(&mut field, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(field.text, "\n");
        press(&mut field, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(field.text, "");
        field.insert("again");
        press(&mut field, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(field.is_empty());
        assert_eq!(field.cursor, 0);
    }

    #[test]
    fn a_move_leaves_the_text_and_up_on_the_first_row_is_not_used() {
        let mut field = TextField::new("one two".to_string());
        let moves = [
            (KeyCode::Left, KeyModifiers::CONTROL, 4),
            (KeyCode::Char('b'), KeyModifiers::ALT, 0),
            (KeyCode::Char('f'), KeyModifiers::ALT, 3),
            (KeyCode::Right, KeyModifiers::NONE, 4),
            (KeyCode::Char('e'), KeyModifiers::CONTROL, 7),
            (KeyCode::Char('a'), KeyModifiers::CONTROL, 0),
            (KeyCode::End, KeyModifiers::NONE, 7),
        ];
        for (code, modifiers, at) in moves {
            assert_eq!(press(&mut field, code, modifiers), Keyed::Moved);
            assert_eq!(field.cursor, at, "{code:?}");
        }
        assert_eq!(field.text, "one two");
        assert_eq!(
            press(&mut field, KeyCode::Up, KeyModifiers::NONE),
            Keyed::Unused
        );
        assert_eq!(
            press(&mut field, KeyCode::Down, KeyModifiers::NONE),
            Keyed::Unused
        );
        assert_eq!(
            press(&mut field, KeyCode::Esc, KeyModifiers::NONE),
            Keyed::Unused
        );
    }

    #[test]
    fn a_view_gives_each_shown_row_as_a_byte_range_of_the_text() {
        // rows "one two " "three " "four"
        let field = TextField {
            text: "one two three four".to_string(),
            cursor: 10,
        };
        let view = field.view(9, 10);
        assert_eq!(view.rows, [0..8, 8..14, 14..18]);
        assert!(view.starts_here);
        assert_eq!(view.cursor, (2, 1));
    }

    #[test]
    fn a_view_shorter_than_the_text_keeps_the_cursors_row_in_view() {
        // rows "ab" "cd" "ef" "gh" "", two at a time
        let mut field = TextField::new("ab\ncd\nef\ngh\n".to_string());
        let view = field.view(20, 2);
        assert_eq!(view.rows, [9..11, 12..12]);
        assert!(!view.starts_here);
        assert_eq!(view.cursor, (0, 1));
        field.cursor = 4;
        let view = field.view(20, 2);
        assert_eq!(view.rows, [3..5, 6..8]);
        assert_eq!(view.cursor, (1, 0));
        field.cursor = 0;
        let view = field.view(20, 2);
        assert_eq!(view.rows, [0..2, 3..5]);
        assert!(view.starts_here);
    }

    #[test]
    fn a_click_lands_before_the_char_under_it_and_a_wide_char_takes_either_column() {
        let field = TextField::new("a日b".to_string());
        let view = field.view(20, 10);
        assert_eq!(field.at(&view, 0, 0), 0);
        assert_eq!(field.at(&view, 1, 0), 1);
        assert_eq!(field.at(&view, 2, 0), 1);
        assert_eq!(field.at(&view, 3, 0), 4);
    }

    #[test]
    fn a_click_past_a_rows_end_lands_at_its_end_and_below_the_rows_at_the_texts_end() {
        // rows "one two " "three" "", the last after a newline
        let field = TextField::new("one two three\n".to_string());
        let view = field.view(9, 10);
        assert_eq!(view.rows, [0..8, 8..13, 14..14]);
        assert_eq!(field.at(&view, 30, 1), 13);
        assert_eq!(field.at(&view, 3, 3), field.text.len());
        // the end of a wrapped row is drawn on the next
        // row, so a click there lands before the break space
        assert_eq!(field.at(&view, 30, 0), 7);
    }

    #[test]
    fn a_click_maps_through_a_view_that_starts_past_the_first_row() {
        // rows "ab" "cd" "ef", the last two shown
        let field = TextField::new("ab\ncd\nef".to_string());
        let view = field.view(20, 2);
        assert!(!view.starts_here);
        assert_eq!(field.at(&view, 1, 0), 4);
        assert_eq!(field.at(&view, 9, 1), 8);
    }

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

    fn wrap_input(text: &str, width: usize) -> Vec<String> {
        wrap_rows(text, width)
            .into_iter()
            .map(|row| text[row].to_string())
            .collect()
    }

    #[test]
    fn input_wraps_at_word_breaks() {
        assert_eq!(
            wrap_input("one two three four", 9),
            ["one two ", "three ", "four"]
        );
    }

    #[test]
    fn a_wrapped_row_never_starts_with_the_space_it_broke_at() {
        assert_eq!(wrap_input("one two three", 7), ["one two ", "three"]);
        let text =
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen";
        for row in wrap_input(text, 40) {
            assert!(!row.starts_with(' '), "{row:?}");
        }
    }

    #[test]
    fn a_break_space_past_the_width_on_the_last_row_puts_the_cursor_on_a_new_row() {
        assert_eq!(wrap_input("abcd ", 4), ["abcd ", ""]);
    }

    #[test]
    fn up_and_down_keep_a_cursor_on_the_break_space_on_its_row() {
        // rows "one two " "three " "four", where the first
        // break space is one column past the width
        let text = "one two three four";
        let rows = wrap_rows(text, 7);
        assert_eq!(cursor_row(&rows, 7), 0);
        assert_eq!(cursor_row(&rows, 8), 1);
        assert_eq!(row_below(text, 7, 7), Some(13));
        assert_eq!(row_above(text, 8 + 3, 7), Some(3));
        assert_eq!(row_above(text, 13, 7), Some(5));
    }

    #[test]
    fn a_word_longer_than_the_row_is_split() {
        assert_eq!(wrap_input("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn input_rows_wrap_by_display_width() {
        assert_eq!(wrap_input("日本語日本", 6), ["日本語", "日本"]);
        assert_eq!(wrap_input("ab日本", 5), ["ab日", "本"]);
        assert_eq!(wrap_input("😀😀😀", 4), ["😀😀", "😀"]);
        for row in wrap_input("日本語 日本語 日本語", 7) {
            assert!(row.width() <= 7);
        }
    }

    #[test]
    fn a_newline_starts_a_new_row_and_each_line_wraps_on_its_own() {
        assert_eq!(wrap_input("ab\n\ncd ef", 4), ["ab", "", "cd ", "ef"]);
        assert_eq!(wrap_input("abcd\nx", 4), ["abcd", "x"]);
        assert_eq!(wrap_input("a\n", 4), ["a", ""]);
    }

    #[test]
    fn moving_between_unwrapped_lines_keeps_the_display_column() {
        let text = "日本語\nabcdef\nx";
        let above = |at| row_above(text, at, 80);
        let below = |at| row_below(text, at, 80);
        assert_eq!(above(0), None);
        assert_eq!(below(text.len()), None);
        // after 日本 (column 4) goes to column 4 below
        assert_eq!(below(6), Some(10 + 4));
        // column 3 falls inside 本, so the cursor lands before it
        assert_eq!(above(10 + 3), Some(3));
        // a short line clamps to its end
        assert_eq!(below(10 + 5), Some(text.len()));
        assert_eq!(above(text.len()), Some(10 + 1));
    }

    #[test]
    fn up_and_down_step_through_the_rows_of_one_wrapped_line() {
        // rows "one two " "three " "four"
        let text = "one two three four";
        assert_eq!(row_below(text, 1, 9), Some(8 + 1));
        assert_eq!(row_below(text, 8 + 1, 9), Some(14 + 1));
        assert_eq!(row_below(text, 14 + 1, 9), None);
        assert_eq!(row_above(text, 14 + 1, 9), Some(8 + 1));
        assert_eq!(row_above(text, 8 + 1, 9), Some(1));
        assert_eq!(row_above(text, 1, 9), None);
    }

    #[test]
    fn a_move_onto_a_shorter_wrapped_row_stays_on_that_row() {
        // rows "one two " "three " "four", and column 7 is past
        // the end of "three ", whose end starts the row "four"
        let text = "one two three four";
        let landed = row_below(text, 7, 9).unwrap();
        let rows = wrap_rows(text, 9);
        assert_eq!(cursor_row(&rows, landed), 1);
        assert_eq!(landed, 13);
    }

    #[test]
    fn a_move_crosses_from_a_wrapped_row_to_the_next_line() {
        // rows "abcd" "ef" "xyz"
        let text = "abcdef\nxyz";
        assert_eq!(row_below(text, 1, 4), Some(4 + 1));
        assert_eq!(row_below(text, 4 + 1, 4), Some(7 + 1));
        assert_eq!(row_above(text, 7 + 2, 4), Some(4 + 2));
    }
}
