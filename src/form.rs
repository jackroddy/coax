//! A form the person fills in, from an ACP `elicitation/create` request in
//! form mode or from a prompt template, and the keys that fill it in.

use std::collections::BTreeMap;

use agent_client_protocol::v1::schema::{
    ElicitationAcceptAction, ElicitationAction, ElicitationContentValue, ElicitationPropertySchema,
    ElicitationSchema, EnumOption, MultiSelectItems,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::prompt::{Keyed, TextField};
use crate::template::{Answer, Kind as TemplateKind, Opt, Template};

pub struct Form {
    /// What the agent asks, the title on the form's rule.
    pub message: String,
    pub fields: Vec<Field>,

    /// The row the cursor is on, an index into `rows()`.
    pub cursor: usize,

    /// Why the last submit was refused.
    pub error: Option<String>,

    /// Whether the form fills a prompt template, where Enter outside a
    /// many-line field finishes the form.
    pub template: bool,

    /// Columns of a text field's text at the last draw.
    pub width: usize,
}

pub struct Field {
    /// The property's name in the schema, which the answer is keyed by.
    pub key: String,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: Kind,

    /// The question this text field takes a typed answer to, for the
    /// `<key>_custom` field the Claude adapter sends after question `<key>`.
    pub other_of: Option<usize>,
}

pub enum Kind {
    /// One choice of `options`.
    Choice {
        options: Vec<Choice>,
        picked: Option<usize>,
    },

    /// Any number of `options`, one flag each.
    Many {
        options: Vec<Choice>,
        picked: Vec<bool>,
    },
    Text {
        text: TextField,
        number: Option<Number>,

        /// Whether Enter adds a line, rather than moving on.
        lines: bool,
    },
    Toggle(bool),
}

pub struct Choice {
    pub value: String,
    pub title: String,
    pub description: Option<String>,

    /// Fixed-width text to show while the option has the cursor.
    pub preview: Option<String>,
}

/// The key under an option's `_meta` where the Claude adapter puts
/// `{ "preview": <text> }`.
const PREVIEW_KEY: &str = "_claude/askUserQuestionOption";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Number {
    Integer,
    Real,
}

/// A row the cursor can be on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// An option of a choice: the field's index, then the option's.
    Option(usize, usize),
    Text(usize),
    Toggle(usize),
    Submit,
}

impl Form {
    pub fn new(message: String, schema: &ElicitationSchema) -> Form {
        let required = schema.required.clone().unwrap_or_default();
        let mut fields: Vec<Field> = schema
            .properties
            .iter()
            .map(|(key, property)| field(key, property, required.contains(key)))
            .collect();
        for index in 0..fields.len() {
            let Some(question) = fields[index].key.strip_suffix(OTHER) else {
                continue;
            };
            fields[index].other_of = fields.iter().position(|field| {
                field.key == question
                    && matches!(field.kind, Kind::Choice { .. } | Kind::Many { .. })
            });
        }
        Form {
            message,
            fields,
            cursor: 0,
            error: None,
            template: false,
            width: 80,
        }
    }

    /// The form for the template `name`: one field per blank, in order,
    /// and a typed Other field after each choice that allows one.
    pub fn template(name: &str, template: &Template) -> Form {
        let message = match &template.description {
            Some(description) => format!("{name} · {description}"),
            None => name.to_string(),
        };
        let mut fields = Vec::new();
        for (key, field) in &template.fields {
            let text = |text: String, lines: bool| Kind::Text {
                text: TextField::new(text),
                number: None,
                lines,
            };
            let kind = match &field.kind {
                TemplateKind::Line { default } => text(default.clone(), false),
                TemplateKind::Text { default } => text(default.clone(), true),
                TemplateKind::List { default, .. } => text(default.join("\n"), true),
                TemplateKind::Choice {
                    options, default, ..
                } => Kind::Choice {
                    picked: default
                        .as_ref()
                        .and_then(|default| options.iter().position(|o| &o.label == default)),
                    options: labelled_choices(options),
                },
                TemplateKind::Many {
                    options, default, ..
                } => Kind::Many {
                    picked: options.iter().map(|o| default.contains(&o.label)).collect(),
                    options: labelled_choices(options),
                },
                TemplateKind::Toggle { default, .. } => Kind::Toggle(*default),
            };
            let index = fields.len();
            fields.push(Field {
                key: key.clone(),
                title: field.label(key).to_string(),
                description: field.help.clone(),
                required: false,
                kind,
                other_of: None,
            });
            if let TemplateKind::Choice { other: true, .. } = field.kind {
                fields.push(Field {
                    key: format!("{key}{OTHER}"),
                    title: "other".to_string(),
                    description: Some("or type another answer".to_string()),
                    required: false,
                    kind: text(String::new(), false),
                    other_of: Some(index),
                });
            }
        }
        Form {
            message,
            fields,
            cursor: 0,
            error: None,
            template: true,
            width: 80,
        }
    }

    /// Each field's answer so far, by blank name, with a choice's typed
    /// Other answer in place of its pick.
    pub fn answers(&self) -> BTreeMap<String, Answer> {
        let mut answers = BTreeMap::new();
        for field in &self.fields {
            let answer = match &field.kind {
                Kind::Choice { options, picked } => {
                    picked.map(|picked| Answer::Text(options[picked].value.clone()))
                }
                Kind::Many { options, picked } => Some(Answer::Items(
                    options
                        .iter()
                        .zip(picked)
                        .filter(|(_, on)| **on)
                        .map(|(option, _)| option.value.clone())
                        .collect(),
                )),
                Kind::Text { text, .. } if text.text.trim().is_empty() => None,
                Kind::Text { text, .. } => Some(Answer::Text(text.text.clone())),
                Kind::Toggle(on) => Some(Answer::Toggle(*on)),
            };
            let key = match field.other_of {
                Some(_) => field.key.strip_suffix(OTHER).unwrap_or(&field.key),
                None => &field.key,
            };
            if let Some(answer) = answer {
                answers.insert(key.to_string(), answer);
            }
        }
        answers
    }

    /// Each question as the person read it, with their answer in words, or
    /// `None` for one left empty. A question's Other text follows its pick.
    pub fn summary(&self) -> Vec<(String, Option<String>)> {
        let mut questions = Vec::new();
        for (index, field) in self.fields.iter().enumerate() {
            if field.other_of.is_some() {
                continue;
            }
            let picked = match &field.kind {
                Kind::Choice { options, picked } => picked.map(|p| options[p].title.clone()),
                Kind::Many { options, picked } => {
                    let titles: Vec<&str> = options
                        .iter()
                        .zip(picked)
                        .filter(|(_, on)| **on)
                        .map(|(option, _)| option.title.as_str())
                        .collect();
                    (!titles.is_empty()).then(|| titles.join(", "))
                }
                Kind::Text { text, .. } => typed(&text.text),
                Kind::Toggle(on) => Some(if *on { "yes" } else { "no" }.to_string()),
            };
            let other = self
                .fields
                .iter()
                .find(|other| other.other_of == Some(index))
                .and_then(|other| match &other.kind {
                    Kind::Text { text, .. } => typed(&text.text),
                    _ => None,
                });
            let answer = match (picked, other) {
                (Some(picked), Some(other)) => Some(format!("{picked} · note: {other}")),
                (picked, other) => picked.or(other),
            };
            let question = field.description.clone().unwrap_or(field.title.clone());
            questions.push((question, answer));
        }
        questions
    }

    /// Add pasted text to the text field the cursor is on, with its
    /// newlines kept only where the field takes lines.
    pub fn paste(&mut self, pasted: &str) {
        let rows = self.rows();
        if let Some(Row::Text(field)) = rows.get(self.cursor).copied()
            && let Kind::Text { text, lines, .. } = &mut self.fields[field].kind
        {
            let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
            if *lines {
                text.insert(&pasted);
            } else {
                text.insert(&pasted.replace('\n', " "));
            }
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (index, field) in self.fields.iter().enumerate() {
            match &field.kind {
                Kind::Choice { options, .. } | Kind::Many { options, .. } => {
                    rows.extend((0..options.len()).map(|option| Row::Option(index, option)));
                }
                Kind::Text { .. } => rows.push(Row::Text(index)),
                Kind::Toggle(_) => rows.push(Row::Toggle(index)),
            }
        }
        rows.push(Row::Submit);
        rows
    }

    /// The preview of the option the cursor is on, with its field's index.
    pub fn focused_preview(&self) -> Option<(usize, &str)> {
        let Row::Option(field, option) = *self.rows().get(self.cursor)? else {
            return None;
        };
        match &self.fields[field].kind {
            Kind::Choice { options, .. } | Kind::Many { options, .. } => {
                Some((field, options[option].preview.as_deref()?))
            }
            _ => None,
        }
    }

    /// Handle a key, and return the answer once the form is submitted or
    /// dismissed.
    pub fn key(&mut self, key: KeyEvent) -> Option<ElicitationAction> {
        let rows = self.rows();
        let row = rows[self.cursor.min(rows.len() - 1)];
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Some(ElicitationAction::Decline),
            KeyCode::Tab => {
                self.cursor = self.next_question_row(&rows, self.cursor);
                return None;
            }
            KeyCode::BackTab => {
                self.cursor = self.previous_question_row(&rows, self.cursor);
                return None;
            }
            KeyCode::Enter if ctrl && self.template => return self.submit(),
            _ => {}
        }
        if let Row::Text(field) = row
            && let Kind::Text { text, lines, .. } = &mut self.fields[field].kind
        {
            let enter = key.code == KeyCode::Enter
                && !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
            if enter && *lines {
                text.insert("\n");
                return None;
            }
            if !enter && text.key(&key, self.width) != Keyed::Unused {
                return None;
            }
        }
        match key.code {
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(rows.len() - 1),
            KeyCode::Enter => match row {
                Row::Submit => return self.submit(),
                Row::Text(_) | Row::Toggle(_) if self.template => return self.submit(),
                Row::Option(field, option) => {
                    self.pick(field, option);
                    if matches!(self.fields[field].kind, Kind::Choice { .. }) {
                        self.cursor = self.next_question_row(&rows, self.cursor);
                    }
                }
                Row::Toggle(field) => self.flip(field),
                Row::Text(_) => self.cursor = self.next_question_row(&rows, self.cursor),
            },
            KeyCode::Char(' ') => match row {
                Row::Option(field, option) => self.pick(field, option),
                Row::Toggle(field) => self.flip(field),
                _ => {}
            },
            _ => {}
        }
        None
    }

    /// The first row after `at` that belongs neither to `at`'s field nor to
    /// that field's other field, or the submit row.
    fn next_question_row(&self, rows: &[Row], at: usize) -> usize {
        let field = row_field(rows[at]);
        rows.iter()
            .skip(at)
            .position(|row| match row_field(*row) {
                Some(next) => Some(next) != field && self.fields[next].other_of != field,
                None => true,
            })
            .map_or(at, |offset| at + offset)
    }

    /// The first row of the question before the one at `at`, or of the
    /// first question when `at` is in it.
    fn previous_question_row(&self, rows: &[Row], at: usize) -> usize {
        let question =
            |row: Row| row_field(row).map(|field| self.fields[field].other_of.unwrap_or(field));
        let here = question(rows[at]);
        let before = rows[..at]
            .iter()
            .rev()
            .find_map(|row| question(*row).filter(|q| Some(*q) != here));
        let target = before.or(here);
        rows.iter()
            .position(|row| question(*row) == target && target.is_some())
            .unwrap_or(at)
    }

    fn pick(&mut self, field: usize, option: usize) {
        match &mut self.fields[field].kind {
            Kind::Choice { picked, .. } => *picked = Some(option),
            Kind::Many { picked, .. } => picked[option] = !picked[option],
            _ => {}
        }
    }

    fn flip(&mut self, field: usize) {
        if let Kind::Toggle(on) = &mut self.fields[field].kind {
            *on = !*on;
        }
    }

    /// The answer as `accept`, or `None` with `error` set and the cursor on
    /// the first field that holds no valid value.
    fn submit(&mut self) -> Option<ElicitationAction> {
        let mut content = BTreeMap::new();
        for (index, field) in self.fields.iter().enumerate() {
            match value(field) {
                Ok(Some(value)) => {
                    content.insert(field.key.clone(), value);
                }
                Ok(None) if !field.required => {}
                Ok(None) => return self.refuse(index, format!("{} needs an answer", field.title)),
                Err(message) => return self.refuse(index, message),
            }
        }
        Some(ElicitationAcceptAction::new().content(content).into())
    }

    /// Refuse to finish: show `message` and put the cursor on `field`.
    pub fn refuse(&mut self, field: usize, message: String) -> Option<ElicitationAction> {
        self.error = Some(message);
        if let Some(row) = self
            .rows()
            .iter()
            .position(|row| row_field(*row) == Some(field))
        {
            self.cursor = row;
        }
        None
    }
}

fn typed(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn field(key: &str, property: &ElicitationPropertySchema, required: bool) -> Field {
    let text = |number| Kind::Text {
        text: TextField::default(),
        number,
        lines: false,
    };
    let (title, description, kind) = match property {
        ElicitationPropertySchema::String(string) => {
            let options = match (&string.one_of, &string.enum_values) {
                (Some(options), _) => Some(choices(options)),
                (None, Some(values)) => Some(plain_choices(values)),
                (None, None) => None,
            };
            let kind = match options {
                Some(options) => {
                    let picked = string
                        .default
                        .as_ref()
                        .and_then(|default| options.iter().position(|c| c.value == *default));
                    Kind::Choice { options, picked }
                }
                None => Kind::Text {
                    text: TextField::new(string.default.clone().unwrap_or_default()),
                    number: None,
                    lines: false,
                },
            };
            (string.title.clone(), string.description.clone(), kind)
        }
        ElicitationPropertySchema::Number(number) => (
            number.title.clone(),
            number.description.clone(),
            text(Some(Number::Real)),
        ),
        ElicitationPropertySchema::Integer(integer) => (
            integer.title.clone(),
            integer.description.clone(),
            text(Some(Number::Integer)),
        ),
        ElicitationPropertySchema::Boolean(boolean) => (
            boolean.title.clone(),
            boolean.description.clone(),
            Kind::Toggle(boolean.default.unwrap_or(false)),
        ),
        ElicitationPropertySchema::Array(array) => {
            let options = match &array.items {
                MultiSelectItems::Titled(titled) => choices(&titled.options),
                MultiSelectItems::String(strings) => plain_choices(&strings.values),
                _ => Vec::new(),
            };
            let defaults = array.default.clone().unwrap_or_default();
            let picked = options
                .iter()
                .map(|c| defaults.contains(&c.value))
                .collect();
            (
                array.title.clone(),
                array.description.clone(),
                Kind::Many { options, picked },
            )
        }
        _ => (None, None, text(None)),
    };
    Field {
        key: key.to_string(),
        title: title.unwrap_or_else(|| key.to_string()),
        description,
        required,
        kind,
        other_of: None,
    }
}

fn choices(options: &[EnumOption]) -> Vec<Choice> {
    options
        .iter()
        .map(|option| Choice {
            value: option.value.clone(),
            title: option.title.clone(),
            description: option.description.clone(),
            preview: preview(option),
        })
        .collect()
}

fn preview(option: &EnumOption) -> Option<String> {
    let preview = option.meta.as_ref()?.get(PREVIEW_KEY)?.get("preview")?;
    preview.as_str().map(str::to_string)
}

/// The ending of the key of a choice's typed Other field.
const OTHER: &str = "_custom";

/// A template's options: each shows its label and answers with its text.
fn labelled_choices(options: &[Opt]) -> Vec<Choice> {
    options
        .iter()
        .map(|option| Choice {
            value: option.text.clone(),
            title: option.label.clone(),
            description: None,
            preview: None,
        })
        .collect()
}

fn plain_choices(values: &[String]) -> Vec<Choice> {
    values
        .iter()
        .map(|value| Choice {
            value: value.clone(),
            title: value.clone(),
            description: None,
            preview: None,
        })
        .collect()
}

/// The field's answer, `None` when it has none, or why its text is not a
/// number.
fn value(field: &Field) -> Result<Option<ElicitationContentValue>, String> {
    Ok(Some(match &field.kind {
        Kind::Choice { options, picked } => match picked {
            Some(picked) => ElicitationContentValue::String(options[*picked].value.clone()),
            None => return Ok(None),
        },
        Kind::Many { options, picked } => {
            let values: Vec<String> = options
                .iter()
                .zip(picked)
                .filter(|(_, on)| **on)
                .map(|(option, _)| option.value.clone())
                .collect();
            if values.is_empty() {
                return Ok(None);
            }
            ElicitationContentValue::StringArray(values)
        }
        Kind::Text { text, .. } if text.text.trim().is_empty() => return Ok(None),
        Kind::Text { text, number, .. } => {
            let text = text.text.trim();
            let not_a = |what: &str| format!("{} needs {what}", field.title);
            match number {
                None => ElicitationContentValue::String(text.to_string()),
                Some(Number::Integer) => ElicitationContentValue::Integer(
                    text.parse().map_err(|_| not_a("a whole number"))?,
                ),
                Some(Number::Real) => {
                    ElicitationContentValue::Number(text.parse().map_err(|_| not_a("a number"))?)
                }
            }
        }
        Kind::Toggle(on) => ElicitationContentValue::Boolean(*on),
    }))
}

pub fn row_field(row: Row) -> Option<usize> {
    match row {
        Row::Option(field, _) | Row::Text(field) | Row::Toggle(field) => Some(field),
        Row::Submit => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::scenario::{Scenario, Step};

    /// The form in `scenarios/questions.json`, shaped as the Claude adapter
    /// sends `AskUserQuestion`.
    fn questions() -> Form {
        let scenario = Scenario::parse(include_str!("../scenarios/questions.json")).unwrap();
        let Step::Elicitation(asked) = &scenario.acts[0].steps[1] else {
            panic!("the second step asks the questions");
        };
        Form::new(asked.message.clone(), &asked.requested_schema)
    }

    fn press(form: &mut Form, code: KeyCode) -> Option<ElicitationAction> {
        form.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn content(action: Option<ElicitationAction>) -> BTreeMap<String, ElicitationContentValue> {
        match action {
            Some(ElicitationAction::Accept(accept)) => accept.content.unwrap_or_default(),
            _ => panic!("the form should be accepted"),
        }
    }

    fn away() -> Form {
        let template: Template = toml::from_str(
            r#"
            text = "{tasks} for {time}. {ask}"
            description = "before I go"
            [fields.tasks]
            kind = "list"
            [fields.time]
            kind = "choice"
            options = ["an hour", "the night"]
            other = true
            [fields.ask]
            kind = "toggle"
            "#,
        )
        .unwrap();
        Form::template("away", &template)
    }

    #[test]
    fn a_template_form_shows_option_labels_picks_the_default_and_answers_with_texts() {
        let template: Template = toml::from_str(
            r#"
            text = "{ask} {langs}"
            [fields.ask]
            kind = "choice"
            default = "yes"
            options = [{ label = "yes", text = "ask now" }, { label = "no", text = "carry on" }]
            [fields.langs]
            kind = "many"
            join = " and "
            default = ["rust"]
            options = [{ label = "rust", text = "in rust" }, "go"]
            "#,
        )
        .unwrap();
        let mut form = Form::template("t", &template);
        let titles = |kind: &Kind| match kind {
            Kind::Choice { options, .. } | Kind::Many { options, .. } => options
                .iter()
                .map(|option| option.title.clone())
                .collect::<Vec<_>>(),
            _ => panic!("options"),
        };
        assert_eq!(titles(&form.fields[0].kind), ["yes", "no"]);
        assert!(matches!(
            form.fields[0].kind,
            Kind::Choice {
                picked: Some(0),
                ..
            }
        ));
        if let Kind::Many { picked, .. } = &mut form.fields[1].kind {
            picked[1] = true;
        }
        assert_eq!(
            template.fill(&form.answers()).unwrap(),
            "ask now in rust and go"
        );
    }

    #[test]
    fn a_template_form_has_a_field_per_blank_and_an_other_field_after_a_choice() {
        let form = away();
        assert_eq!(form.message, "away · before I go");
        let keys: Vec<&str> = form.fields.iter().map(|field| field.key.as_str()).collect();
        assert_eq!(keys, ["tasks", "time", "time_custom", "ask"]);
        assert_eq!(form.fields[2].other_of, Some(1));
        assert!(matches!(
            form.fields[0].kind,
            Kind::Text { lines: true, .. }
        ));
    }

    #[test]
    fn tab_and_shift_tab_move_a_question_at_a_time_and_enter_adds_a_line_in_a_list() {
        let mut form = away();
        assert_eq!(press(&mut form, KeyCode::Enter), None);
        assert!(matches!(&form.fields[0].kind, Kind::Text { text, .. } if text.text == "\n"));
        press(&mut form, KeyCode::Tab);
        assert_eq!(form.rows()[form.cursor], Row::Option(1, 0));
        press(&mut form, KeyCode::Tab);
        assert_eq!(form.rows()[form.cursor], Row::Toggle(3));
        press(&mut form, KeyCode::BackTab);
        assert_eq!(form.rows()[form.cursor], Row::Option(1, 0));
        press(&mut form, KeyCode::BackTab);
        assert_eq!(form.rows()[form.cursor], Row::Text(0));
    }

    #[test]
    fn enter_on_a_toggle_or_ctrl_enter_anywhere_finishes_a_template_form() {
        let mut form = away();
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        assert!(matches!(
            form.key(ctrl_enter),
            Some(ElicitationAction::Accept(_))
        ));
        let mut form = away();
        press(&mut form, KeyCode::Tab);
        press(&mut form, KeyCode::Tab);
        assert!(matches!(
            press(&mut form, KeyCode::Enter),
            Some(ElicitationAction::Accept(_))
        ));
    }

    #[test]
    fn the_answers_put_a_typed_other_in_place_of_the_pick() {
        let mut form = away();
        form.paste("fix it");
        press(&mut form, KeyCode::Tab);
        press(&mut form, KeyCode::Enter);
        let answers = form.answers();
        assert_eq!(answers["time"], Answer::Text("an hour".to_string()));
        assert_eq!(answers["tasks"], Answer::Text("fix it".to_string()));
        form.cursor = form
            .rows()
            .iter()
            .position(|row| *row == Row::Text(2))
            .unwrap();
        form.paste("a\nwhile");
        assert_eq!(form.answers()["time"], Answer::Text("a while".to_string()));
        assert_eq!(form.answers()["ask"], Answer::Toggle(false));
    }

    fn type_in(form: &mut Form, text: &str) {
        for ch in text.chars() {
            press(form, KeyCode::Char(ch));
        }
    }

    fn typed_text(form: &Form, field: usize) -> &str {
        match &form.fields[field].kind {
            Kind::Text { text, .. } => &text.text,
            _ => panic!("field {field} is not a text field"),
        }
    }

    #[test]
    fn shift_enter_adds_a_line_in_any_text_field_and_enter_still_moves_on() {
        let mut form = questions();
        form.cursor = 2;
        type_in(&mut form, "a");
        form.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        type_in(&mut form, "b");
        assert_eq!(typed_text(&form, 1), "a\nb");
        assert_eq!(form.cursor, 2);
        press(&mut form, KeyCode::Enter);
        assert_ne!(form.cursor, 2);
    }

    #[test]
    fn the_cursor_keys_edit_a_form_text_field_in_the_middle_of_its_text() {
        let mut form = questions();
        form.cursor = 2;
        type_in(&mut form, "ac");
        press(&mut form, KeyCode::Left);
        type_in(&mut form, "b");
        assert_eq!(typed_text(&form, 1), "abc");
        press(&mut form, KeyCode::Home);
        press(&mut form, KeyCode::Delete);
        assert_eq!(typed_text(&form, 1), "bc");
        press(&mut form, KeyCode::End);
        press(&mut form, KeyCode::Backspace);
        assert_eq!(typed_text(&form, 1), "b");
        type_in(&mut form, " x");
        assert_eq!(typed_text(&form, 1), "b x");
    }

    #[test]
    fn up_in_a_text_field_moves_a_row_within_it_before_it_leaves_the_field() {
        let mut form = questions();
        form.cursor = 2;
        // rows "one " "two"
        type_in(&mut form, "one two");
        form.width = 4;
        press(&mut form, KeyCode::Up);
        assert_eq!(form.cursor, 2);
        assert_eq!(typed_text(&form, 1), "one two");
        press(&mut form, KeyCode::Up);
        assert_eq!(form.cursor, 1);
    }

    #[test]
    fn an_ask_user_question_form_has_a_choice_a_note_and_a_multiple_choice() {
        let form = questions();
        let titles: Vec<&str> = form.fields.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(titles, ["Storage", "Other", "Platforms", "Other"]);
        assert!(
            matches!(form.fields[0].kind, Kind::Choice { ref options, picked: None } if options.len() == 2)
        );
        assert!(matches!(
            form.fields[1].kind,
            Kind::Text { number: None, .. }
        ));
        assert!(
            matches!(form.fields[2].kind, Kind::Many { ref options, .. } if options.len() == 3)
        );
        assert_eq!(form.rows().len(), 2 + 1 + 3 + 1 + 1);
    }

    #[test]
    fn enter_on_a_choice_picks_it_and_moves_past_its_other_field_to_the_next_question() {
        let mut form = questions();
        press(&mut form, KeyCode::Down);
        assert_eq!(press(&mut form, KeyCode::Enter), None);
        assert!(matches!(
            form.fields[0].kind,
            Kind::Choice {
                picked: Some(1),
                ..
            }
        ));
        assert_eq!(form.rows()[form.cursor], Row::Option(2, 0));
        press(&mut form, KeyCode::Up);
        assert_eq!(form.rows()[form.cursor], Row::Text(1));
        press(&mut form, KeyCode::Enter);
        assert_eq!(form.rows()[form.cursor], Row::Option(2, 0));
    }

    #[test]
    fn each_custom_field_belongs_to_the_question_it_is_named_for() {
        let form = questions();
        let other_of: Vec<Option<usize>> = form.fields.iter().map(|f| f.other_of).collect();
        assert_eq!(other_of, [None, Some(0), None, Some(2)]);
    }

    #[test]
    fn enter_in_a_multiple_choice_toggles_and_stays() {
        let mut form = questions();
        form.cursor = 3;
        press(&mut form, KeyCode::Enter);
        press(&mut form, KeyCode::Char(' '));
        assert!(
            matches!(&form.fields[2].kind, Kind::Many { picked, .. } if picked == &[false, false, false])
        );
        press(&mut form, KeyCode::Enter);
        press(&mut form, KeyCode::Down);
        press(&mut form, KeyCode::Enter);
        assert!(
            matches!(&form.fields[2].kind, Kind::Many { picked, .. } if picked == &[true, true, false])
        );
        assert_eq!(form.cursor, 4);
    }

    #[test]
    fn submit_sends_the_picked_values_and_any_text_typed() {
        let mut form = questions();
        press(&mut form, KeyCode::Down);
        press(&mut form, KeyCode::Enter);
        press(&mut form, KeyCode::Up);
        for ch in "on SSD".chars() {
            press(&mut form, KeyCode::Char(ch));
        }
        form.cursor = form.rows().len() - 1;
        let content = content(press(&mut form, KeyCode::Enter));
        assert_eq!(
            content.get("question_0"),
            Some(&ElicitationContentValue::String("Disk".to_string()))
        );
        assert_eq!(
            content.get("question_0_custom"),
            Some(&ElicitationContentValue::String("on SSD".to_string()))
        );
        assert!(!content.contains_key("question_1"));
    }

    #[test]
    fn the_summary_gives_each_question_its_picks_and_note_and_leaves_an_empty_one_unanswered() {
        let mut form = questions();
        press(&mut form, KeyCode::Down);
        press(&mut form, KeyCode::Enter);
        press(&mut form, KeyCode::Up);
        form.paste("on SSD");
        assert_eq!(
            form.summary(),
            [
                (
                    "Where should the cache live?".to_string(),
                    Some("Disk · note: on SSD".to_string())
                ),
                (form.fields[2].description.clone().unwrap(), None),
            ]
        );
        form.cursor = 3;
        press(&mut form, KeyCode::Char(' '));
        form.cursor = 5;
        press(&mut form, KeyCode::Char(' '));
        assert_eq!(form.summary()[1].1.as_deref(), Some("Linux, Windows"));
    }

    #[test]
    fn an_options_preview_is_read_from_its_meta_and_shows_only_while_it_has_the_cursor() {
        let mut form = questions();
        let Kind::Choice { options, .. } = &form.fields[0].kind else {
            panic!("the first field is a choice");
        };
        assert_eq!(options[0].preview, None);
        assert!(options[1].preview.as_deref().unwrap().starts_with("state/"));
        assert_eq!(form.focused_preview(), None);
        press(&mut form, KeyCode::Down);
        assert!(matches!(form.focused_preview(), Some((0, text)) if text.starts_with("state/")));
        press(&mut form, KeyCode::Down);
        assert_eq!(form.focused_preview(), None);
    }

    #[test]
    fn esc_dismisses_the_form() {
        let mut form = questions();
        assert!(matches!(
            press(&mut form, KeyCode::Esc),
            Some(ElicitationAction::Decline)
        ));
    }

    #[test]
    fn a_required_field_or_a_bad_number_refuses_the_submit_and_moves_to_the_field() {
        let schema: ElicitationSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer", "title": "Count"},
                "name": {"type": "string", "title": "Name"}
            },
            "required": ["name"]
        }))
        .unwrap();
        let mut form = Form::new("Details".to_string(), &schema);
        press(&mut form, KeyCode::Char('x'));
        form.cursor = 2;
        assert_eq!(press(&mut form, KeyCode::Enter), None);
        assert_eq!(form.error.as_deref(), Some("Count needs a whole number"));
        assert_eq!(form.cursor, 0);
        press(&mut form, KeyCode::Backspace);
        press(&mut form, KeyCode::Char('3'));
        form.cursor = 2;
        assert_eq!(press(&mut form, KeyCode::Enter), None);
        assert_eq!(form.error.as_deref(), Some("Name needs an answer"));
        assert_eq!(form.cursor, 1);
        press(&mut form, KeyCode::Char('a'));
        form.cursor = 2;
        let content = content(press(&mut form, KeyCode::Enter));
        assert_eq!(
            content.get("count"),
            Some(&ElicitationContentValue::Integer(3))
        );
    }
}
