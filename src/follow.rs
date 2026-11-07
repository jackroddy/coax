use std::path::PathBuf;

use agent_client_protocol::v1::schema::{
    SessionUpdate, ToolCall, ToolCallContent, ToolCallStatus, ToolKind,
};
use decade_channel::code::{Mark, Show};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Show(Show),
    Mark(Mark),
}

pub fn from_update(update: &SessionUpdate, calls: &[ToolCall]) -> Vec<Action> {
    match update {
        SessionUpdate::ToolCall(call) => from_call(call),
        SessionUpdate::ToolCallUpdate(update) => calls
            .iter()
            .find(|call| call.tool_call_id == update.tool_call_id)
            .map(from_call)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn from_call(call: &ToolCall) -> Vec<Action> {
    match call.kind {
        ToolKind::Read => call
            .locations
            .last()
            .map(|location| {
                vec![Action::Show(Show {
                    path: location.path.clone(),
                    line: location.line,
                })]
            })
            .unwrap_or_default(),
        ToolKind::Edit if call.status == ToolCallStatus::Completed => call
            .content
            .iter()
            .filter_map(|content| match content {
                ToolCallContent::Diff(diff) => Some(diff),
                _ => None,
            })
            .flat_map(|diff| {
                let lines = changed_lines(diff.old_text.as_deref(), &diff.new_text);
                let mut actions = vec![Action::Show(Show {
                    path: diff.path.clone(),
                    line: lines.first().map(|(from, _)| *from),
                })];
                if !lines.is_empty() {
                    actions.push(Action::Mark(Mark {
                        path: diff.path.clone(),
                        lines,
                    }));
                }
                actions
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn from_read(path: PathBuf, line: Option<u32>) -> Action {
    Action::Show(Show { path, line })
}

pub fn changed_lines(old: Option<&str>, new: &str) -> Vec<(u32, u32)> {
    let new_lines: Vec<&str> = new.lines().collect();
    if new_lines.is_empty() {
        return Vec::new();
    }
    let Some(old) = old else {
        return vec![(1, new_lines.len() as u32)];
    };
    let old_lines: Vec<&str> = old.lines().collect();
    let prefix = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old_lines[prefix..]
        .iter()
        .rev()
        .zip(new_lines[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let from = prefix + 1;
    let to = new_lines.len() - suffix;
    if to < from {
        return vec![(
            from.min(new_lines.len()) as u32,
            from.min(new_lines.len()) as u32,
        )];
    }
    vec![(from as u32, to as u32)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::{
        Diff, ToolCallLocation, ToolCallUpdate, ToolCallUpdateFields,
    };

    #[test]
    fn a_read_with_a_location_shows_that_file_at_its_line() {
        let call = ToolCall::new("c1", "Read gpu.rs")
            .kind(ToolKind::Read)
            .locations(vec![ToolCallLocation::new("src/gpu.rs").line(40)]);
        let actions = from_update(&SessionUpdate::ToolCall(call), &[]);
        assert_eq!(
            actions,
            vec![Action::Show(Show {
                path: "src/gpu.rs".into(),
                line: Some(40)
            })]
        );
    }

    #[test]
    fn a_completed_edit_with_a_diff_shows_and_marks_the_changed_lines() {
        let diff =
            Diff::new("a.rs", "one\ntwo\nTHREE\nFOUR\nfive\n").old_text("one\ntwo\nthree\nfive\n");
        let call = ToolCall::new("c2", "Edit a.rs")
            .kind(ToolKind::Edit)
            .status(ToolCallStatus::Completed)
            .content(vec![ToolCallContent::from(diff)]);
        let actions = from_update(&SessionUpdate::ToolCall(call), &[]);
        assert_eq!(
            actions,
            vec![
                Action::Show(Show {
                    path: "a.rs".into(),
                    line: Some(3)
                }),
                Action::Mark(Mark {
                    path: "a.rs".into(),
                    lines: vec![(3, 4)]
                }),
            ]
        );
    }

    #[test]
    fn an_update_is_judged_by_the_call_it_belongs_to() {
        let call = ToolCall::new("c3", "Read x")
            .kind(ToolKind::Read)
            .locations(vec![ToolCallLocation::new("x.rs").line(7)]);
        let update = ToolCallUpdate::new(
            "c3",
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        );
        let actions = from_update(
            &SessionUpdate::ToolCallUpdate(update),
            std::slice::from_ref(&call),
        );
        assert_eq!(actions.len(), 1);
        let actions = from_update(
            &SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                "unknown",
                ToolCallUpdateFields::new(),
            )),
            &[call],
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn a_new_file_marks_every_line_and_an_unchanged_file_marks_one() {
        assert_eq!(changed_lines(None, "a\nb\nc\n"), vec![(1, 3)]);
        assert_eq!(
            changed_lines(Some("a\nb\n"), "a\nb\n"),
            vec![(3, 3)].into_iter().map(|_| (2, 2)).collect::<Vec<_>>()
        );
        assert!(changed_lines(Some("a"), "").is_empty());
    }

    #[test]
    fn a_search_or_execute_call_does_nothing() {
        let call = ToolCall::new("c4", "grep")
            .kind(ToolKind::Search)
            .locations(vec![ToolCallLocation::new("x.rs").line(1)]);
        assert!(from_update(&SessionUpdate::ToolCall(call), &[]).is_empty());
    }
}
