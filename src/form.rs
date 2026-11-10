//! A form the agent asks the person to fill in, from an ACP `elicitation/create`
//! request in form mode, and the keys that fill it in.

use std::collections::BTreeMap;

use agent_client_protocol::v1::schema::{
    ElicitationAcceptAction, ElicitationAction, ElicitationContentValue, ElicitationPropertySchema,
    ElicitationSchema, EnumOption, MultiSelectItems,
};
use crossterm::event::{KeyCode, KeyEvent};

pub struct Form {
    /// What the agent asks, the box's title.
    pub message: String,
    pub fields: Vec<Field>,

    /// The row the cursor is on, an index into `rows()`.
    pub cursor: usize,

    /// Why the last submit was refused.
    pub error: Option<String>,
}

pub struct Field {
    /// The property's name in the schema, which the answer is keyed by.
    pub key: String,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: Kind,
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
        text: String,
        number: Option<Number>,
    },
    Toggle(bool),
}

pub struct Choice {
    pub value: String,
    pub title: String,
    pub description: Option<String>,
}

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
        let fields = schema
            .properties
            .iter()
            .map(|(key, property)| field(key, property, required.contains(key)))
            .collect();
        Form {
            message,
            fields,
            cursor: 0,
            error: None,
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

    /// Handle a key, and return the answer once the form is submitted or
    /// dismissed.
    pub fn key(&mut self, key: KeyEvent) -> Option<ElicitationAction> {
        let rows = self.rows();
        let row = rows[self.cursor.min(rows.len() - 1)];
        match key.code {
            KeyCode::Esc => return Some(ElicitationAction::Decline),
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(rows.len() - 1),
            KeyCode::Enter => match row {
                Row::Submit => return self.submit(),
                Row::Option(field, option) => {
                    self.pick(field, option);
                    if matches!(self.fields[field].kind, Kind::Choice { .. }) {
                        self.cursor = next_field_row(&rows, self.cursor);
                    }
                }
                Row::Toggle(field) => self.flip(field),
                Row::Text(_) => self.cursor = next_field_row(&rows, self.cursor),
            },
            KeyCode::Char(' ') if !matches!(row, Row::Text(_)) => match row {
                Row::Option(field, option) => self.pick(field, option),
                Row::Toggle(field) => self.flip(field),
                _ => {}
            },
            KeyCode::Char(ch) => {
                if let Row::Text(field) = row
                    && let Kind::Text { text, .. } = &mut self.fields[field].kind
                {
                    text.push(ch);
                }
            }
            KeyCode::Backspace => {
                if let Row::Text(field) = row
                    && let Kind::Text { text, .. } = &mut self.fields[field].kind
                {
                    text.pop();
                }
            }
            _ => {}
        }
        None
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

    fn refuse(&mut self, field: usize, message: String) -> Option<ElicitationAction> {
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

fn field(key: &str, property: &ElicitationPropertySchema, required: bool) -> Field {
    let text = |number| Kind::Text {
        text: String::new(),
        number,
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
                    text: string.default.clone().unwrap_or_default(),
                    number: None,
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
    }
}

fn choices(options: &[EnumOption]) -> Vec<Choice> {
    options
        .iter()
        .map(|option| Choice {
            value: option.value.clone(),
            title: option.title.clone(),
            description: option.description.clone(),
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
        Kind::Text { text, .. } if text.trim().is_empty() => return Ok(None),
        Kind::Text { text, number } => {
            let text = text.trim();
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

fn row_field(row: Row) -> Option<usize> {
    match row {
        Row::Option(field, _) | Row::Text(field) | Row::Toggle(field) => Some(field),
        Row::Submit => None,
    }
}

/// The first row of the field after the one row `at` belongs to, or the
/// submit row.
fn next_field_row(rows: &[Row], at: usize) -> usize {
    let field = row_field(rows[at]);
    rows.iter()
        .skip(at)
        .position(|row| row_field(*row) != field)
        .map_or(at, |offset| at + offset)
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
    fn enter_on_a_choice_picks_it_and_moves_to_the_next_field() {
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
        assert_eq!(form.rows()[form.cursor], Row::Text(1));
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
