//! Prompt templates: named prompts from `config.toml` with blanks to fill.

use std::ops::Range;

use serde::Deserialize;

/// A prompt from `[templates.<name>]`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Template {
    /// The prompt, with each blank written `{name}`.
    pub text: String,

    /// What the command list says about the template.
    pub description: Option<String>,
}

impl Template {
    /// The text as it goes into the prompt, with `{{` and `}}` as single
    /// braces, and the names of its blanks in order of first appearance.
    pub fn fill(&self) -> (String, Vec<String>) {
        let mut text = String::new();
        let mut blanks: Vec<String> = Vec::new();
        let mut rest = self.text.as_str();
        while let Some(ch) = rest.chars().next() {
            if let Some(after) = rest.strip_prefix("{{") {
                text.push('{');
                rest = after;
            } else if let Some(after) = rest.strip_prefix("}}") {
                text.push('}');
                rest = after;
            } else if let Some(name) = blank_at(rest) {
                if !blanks.iter().any(|blank| blank == name) {
                    blanks.push(name.to_string());
                }
                text.push_str(&rest[..name.len() + 2]);
                rest = &rest[name.len() + 2..];
            } else {
                text.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
        (text, blanks)
    }

    /// The line the command list shows: the description, or else the
    /// text's first line.
    pub fn summary(&self) -> &str {
        self.description
            .as_deref()
            .unwrap_or_else(|| self.text.lines().next().unwrap_or(""))
    }
}

/// The name of the blank `text` starts with, if it starts with one.
fn blank_at(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('{')?;
    let end = inner.find('}')?;
    let name = &inner[..end];
    let fits = |ch: char| ch.is_alphanumeric() || ch == '_' || ch == '-';
    (!name.is_empty() && name.chars().all(fits)).then_some(name)
}

/// Where each `{name}` for a name in `blanks` sits in `text`, in order.
pub fn spots(text: &str, blanks: &[String]) -> Vec<Range<usize>> {
    let mut found: Vec<Range<usize>> = blanks
        .iter()
        .flat_map(|name| {
            let written = format!("{{{name}}}");
            text.match_indices(&written)
                .map(|(at, _)| at..at + written.len())
                .collect::<Vec<_>>()
        })
        .collect();
    found.sort_by_key(|spot| spot.start);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(text: &str) -> Template {
        Template {
            text: text.to_string(),
            description: None,
        }
    }

    #[test]
    fn a_template_lists_its_blanks_once_each_in_order_of_first_use() {
        let (text, blanks) = template("{tasks}\n\naway for {time}, {tasks} again").fill();
        assert_eq!(text, "{tasks}\n\naway for {time}, {tasks} again");
        assert_eq!(blanks, ["tasks", "time"]);
    }

    #[test]
    fn doubled_braces_are_single_braces_and_never_a_blank() {
        let (text, blanks) = template("use {{x}} and {it} but not {a b} or {}").fill();
        assert_eq!(text, "use {x} and {it} but not {a b} or {}");
        assert_eq!(blanks, ["it"]);
    }

    #[test]
    fn a_template_with_no_blanks_fills_to_its_own_text() {
        let (text, blanks) = template("Do commits, cleanups, and foam bookkeeping").fill();
        assert_eq!(text, "Do commits, cleanups, and foam bookkeeping");
        assert!(blanks.is_empty());
    }

    #[test]
    fn the_spots_of_the_blanks_come_in_the_order_they_sit_in_the_text() {
        let blanks = ["time".to_string(), "tasks".to_string()];
        let text = "{tasks} for {time}, {tasks}";
        assert_eq!(spots(text, &blanks), [0..7, 12..18, 20..27]);
        assert!(spots("done for now", &blanks).is_empty());
    }

    #[test]
    fn the_summary_is_the_description_or_else_the_first_line() {
        let mut away = template("{tasks}\n\nI'm away");
        assert_eq!(away.summary(), "{tasks}");
        away.description = Some("before I step out".to_string());
        assert_eq!(away.summary(), "before I step out");
    }
}
