//! A range of the transcript marked by a drag, and the text a copy of it
//! takes.

use std::ops::Range;

use cmr::Join;
use ratatui::layout::{Position, Rect};
use unicode_width::UnicodeWidthChar;

/// The text a transcript line holds: the column it starts at, the text, and
/// how the line follows the one above.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineText {
    pub col: usize,
    pub text: String,
    pub join: Join,
}

impl LineText {
    /// Each character with its first column and its width.
    fn cells(&self) -> impl Iterator<Item = (char, usize, usize)> + '_ {
        let mut col = self.col;
        self.text.chars().map(move |ch| {
            let width = ch.width().unwrap_or(0);
            col += width;
            (ch, col - width, width)
        })
    }
}

/// A place in the transcript: a line among all of its lines, and a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: usize,
    pub col: usize,
}

/// A range marked by a drag, from where the drag started to where it got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,

    /// Where the pointer is while the drag goes on.
    pub pointer: Option<Position>,
}

impl Selection {
    /// The ends, the earlier first.
    pub fn ends(&self) -> (Point, Point) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    /// Whether the cell `width` columns wide at `col` of `line` is selected.
    fn picks(&self, line: usize, col: usize, width: usize) -> bool {
        let (from, to) = self.ends();
        (from.line..=to.line).contains(&line)
            && (line > from.line || col + width > from.col)
            && (line < to.line || col <= to.col)
    }

    /// The columns of `text`, on `line`, that the selection covers.
    pub fn covers(&self, line: usize, text: &LineText) -> Option<Range<usize>> {
        let mut picked = text
            .cells()
            .filter(|&(_, col, width)| self.picks(line, col, width));
        let (_, first, width) = picked.next()?;
        let end = picked
            .last()
            .map_or(first + width, |(_, col, width)| col + width);
        Some(first..end)
    }

    /// The selected text of `lines`, where `lines[n]` is line `n`. A line
    /// that goes on from the one above joins it with a space or with
    /// nothing, and every other line starts after a newline.
    pub fn copy(&self, lines: &[Option<LineText>]) -> String {
        let (from, to) = self.ends();
        let mut out = String::new();
        let mut first = true;
        for line in from.line..=to.line {
            let Some(Some(text)) = lines.get(line) else {
                continue;
            };
            if !first {
                out.push_str(match text.join {
                    Join::Break => "\n",
                    Join::Space => " ",
                    Join::Word => "",
                });
            }
            first = false;
            out.extend(
                text.cells()
                    .filter(|&(_, col, width)| self.picks(line, col, width))
                    .map(|(ch, ..)| ch),
            );
        }
        out
    }
}

/// The transcript's text as the last draw laid it out.
#[derive(Default)]
pub struct Texts {
    /// Where the transcript was drawn, its scroll bar left out.
    pub area: Rect,

    /// The first line shown.
    pub start: usize,

    /// The text of each of the transcript's lines, `None` for a line that
    /// holds none or was not laid out.
    pub lines: Vec<Option<LineText>>,
}

impl Texts {
    /// The point under `position`, when it is on a line that holds text.
    pub fn text_at(&self, position: Position) -> Option<Point> {
        if !self.area.contains(position) {
            return None;
        }
        let point = self.nearest(position);
        self.lines.get(point.line)?.as_ref()?;
        Some(point)
    }

    /// `nearest`, for the lines the last draw showed.
    pub fn nearest(&self, position: Position) -> Point {
        let end = self.start
            + (self.area.height as usize).min(self.lines.len().saturating_sub(self.start));
        nearest(self.area, self.start..end, position)
    }
}

/// The point nearest `position` of a transcript drawn in `area` with the
/// lines `shown`: the start of the first line when it is above them, and
/// the end of the last when it is below.
pub fn nearest(area: Rect, shown: Range<usize>, position: Position) -> Point {
    if position.y < area.y {
        return Point {
            line: shown.start,
            col: 0,
        };
    }
    let line = shown.start + position.y.saturating_sub(area.y) as usize;
    if line >= shown.end {
        return Point {
            line: shown.end.saturating_sub(1),
            col: usize::MAX,
        };
    }
    let col = if position.x >= area.right() {
        usize::MAX
    } else {
        position.x.saturating_sub(area.x) as usize
    };
    Point { line, col }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(col: usize, text: &str, join: Join) -> Option<LineText> {
        Some(LineText {
            col,
            text: text.to_string(),
            join,
        })
    }

    fn selection(from: (usize, usize), to: (usize, usize)) -> Selection {
        Selection {
            anchor: Point {
                line: from.0,
                col: from.1,
            },
            head: Point {
                line: to.0,
                col: to.1,
            },
            pointer: None,
        }
    }

    #[test]
    fn a_selection_runs_from_the_earlier_end_whichever_way_it_was_dragged() {
        let lines = [text(2, "hello world", Join::Break)];
        assert_eq!(selection((0, 4), (0, 8)).copy(&lines), "llo w");
        assert_eq!(selection((0, 8), (0, 4)).copy(&lines), "llo w");
    }

    #[test]
    fn a_line_with_no_text_adds_nothing_to_a_copy() {
        let lines = [
            text(0, "one", Join::Break),
            None,
            text(0, "two", Join::Break),
        ];
        assert_eq!(selection((0, 0), (2, 9)).copy(&lines), "one\ntwo");
    }

    #[test]
    fn a_selection_covers_only_the_columns_that_hold_text() {
        let line = text(2, "abc", Join::Break).unwrap();
        assert_eq!(selection((0, 0), (1, 0)).covers(0, &line), Some(2..5));
        assert_eq!(selection((0, 3), (0, 3)).covers(0, &line), Some(3..4));
        assert_eq!(selection((0, 6), (1, 0)).covers(0, &line), None);
    }

    #[test]
    fn a_wide_character_is_picked_from_either_of_its_columns() {
        let lines = [text(0, "a界b", Join::Break)];
        assert_eq!(selection((0, 2), (0, 2)).copy(&lines), "界");
        assert_eq!(selection((0, 1), (0, 1)).copy(&lines), "界");
    }

    #[test]
    fn a_point_above_or_below_the_area_is_the_start_or_end_of_the_edge_line() {
        let area = Rect::new(0, 2, 10, 5);
        let end = Point {
            line: 34,
            col: usize::MAX,
        };
        assert_eq!(
            nearest(area, 30..35, Position::new(4, 0)),
            Point { line: 30, col: 0 }
        );
        assert_eq!(nearest(area, 30..35, Position::new(4, 9)), end);
        assert_eq!(
            nearest(area, 30..35, Position::new(4, 3)),
            Point { line: 31, col: 4 }
        );
        assert_eq!(nearest(area, 30..33, Position::new(4, 5)).line, 32);
    }
}
