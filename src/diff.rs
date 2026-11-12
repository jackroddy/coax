use agent_client_protocol::v1::schema::ToolCallContent;

/// The most cells of the table that finds a diff's common lines. Past it the
/// old lines show as removed and the new as added.
const DIFF_CELLS: usize = 4_000_000;

/// How a line of a diff stands: `' '` unchanged, `'-'` removed, `'+'` added.
pub fn line_diff<'a>(old: &'a str, new: &'a str) -> Vec<(char, &'a str)> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (old_mid, new_mid) = (
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );

    let mut out: Vec<(char, &str)> = old[..prefix].iter().map(|line| (' ', *line)).collect();
    if old_mid.len() * new_mid.len() > DIFF_CELLS {
        out.extend(old_mid.iter().map(|line| ('-', *line)));
        out.extend(new_mid.iter().map(|line| ('+', *line)));
    } else {
        // common[i][j] is the length of the longest run of shared lines in
        // old_mid[i..] and new_mid[j..], stored row by row
        let columns = new_mid.len() + 1;
        let mut common = vec![0u32; (old_mid.len() + 1) * columns];
        for i in (0..old_mid.len()).rev() {
            for j in (0..new_mid.len()).rev() {
                common[i * columns + j] = if old_mid[i] == new_mid[j] {
                    common[(i + 1) * columns + j + 1] + 1
                } else {
                    common[(i + 1) * columns + j].max(common[i * columns + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < old_mid.len() && j < new_mid.len() {
            if old_mid[i] == new_mid[j] {
                out.push((' ', old_mid[i]));
                i += 1;
                j += 1;
            } else if common[(i + 1) * columns + j] >= common[i * columns + j + 1] {
                out.push(('-', old_mid[i]));
                i += 1;
            } else {
                out.push(('+', new_mid[j]));
                j += 1;
            }
        }
        out.extend(old_mid[i..].iter().map(|line| ('-', *line)));
        out.extend(new_mid[j..].iter().map(|line| ('+', *line)));
    }
    out.extend(old[old.len() - suffix..].iter().map(|line| (' ', *line)));
    out
}

/// How many lines the diffs in `content` add and remove, counted from the
/// same line diff the expanded view shows.
pub fn size(content: &[ToolCallContent]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for content in content {
        if let ToolCallContent::Diff(diff) = content {
            for (mark, _) in line_diff(diff.old_text.as_deref().unwrap_or(""), &diff.new_text) {
                match mark {
                    '+' => added += 1,
                    '-' => removed += 1,
                    _ => {}
                }
            }
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::Diff;

    use super::*;

    #[test]
    fn a_line_diff_marks_removed_added_and_unchanged_lines() {
        assert_eq!(
            line_diff("a\nb\nc", "a\nx\nc"),
            [(' ', "a"), ('-', "b"), ('+', "x"), (' ', "c")]
        );
        assert_eq!(
            line_diff("a\nb", "a\nb\nc"),
            [(' ', "a"), (' ', "b"), ('+', "c")]
        );
        assert_eq!(line_diff("", "a"), [('+', "a")]);
        assert_eq!(line_diff("a\nb", ""), [('-', "a"), ('-', "b")]);
    }

    #[test]
    fn a_line_diff_keeps_the_lines_two_ends_share_when_the_middle_moved() {
        assert_eq!(
            line_diff("a\nb\nc\nd\ne", "a\nc\nb\nd\ne"),
            [
                (' ', "a"),
                ('-', "b"),
                (' ', "c"),
                ('+', "b"),
                (' ', "d"),
                (' ', "e")
            ]
        );
    }

    #[test]
    fn the_size_of_an_edit_counts_the_lines_that_changed_and_not_the_whole_texts() {
        let mut diff = Diff::new("src/main.rs", "a\nx\nc\nd");
        diff.old_text = Some("a\nb\nc\nd".to_string());
        assert_eq!(size(&[ToolCallContent::Diff(diff)]), (1, 1));
        assert_eq!(
            size(&[ToolCallContent::Diff(Diff::new("new.rs", "one\ntwo"))]),
            (2, 0)
        );
    }
}
