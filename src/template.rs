//! Prompt templates: named prompts from `config.toml` whose blanks a form
//! fills in.

use std::collections::BTreeMap;

use serde::Deserialize;

/// A prompt from `[templates.<name>]`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RawTemplate")]
pub struct Template {
    /// The prompt, with each blank written `{name}` and `{{`, `}}` for
    /// literal braces.
    pub text: String,

    /// What the command list says about the template.
    pub description: Option<String>,

    /// Whether the form opens with the filled text shown under its fields.
    pub preview: bool,

    /// Each blank's name and field, in order of first appearance.
    pub fields: Vec<(String, Field)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTemplate {
    text: String,
    description: Option<String>,
    #[serde(default)]
    preview: bool,
    #[serde(default)]
    fields: BTreeMap<String, Field>,
}

impl TryFrom<RawTemplate> for Template {
    type Error = String;

    fn try_from(raw: RawTemplate) -> Result<Template, String> {
        let names = blanks(&raw.text);
        let mut described = raw.fields;
        if let Some(name) = described.keys().find(|name| !names.contains(name)) {
            return Err(format!("field {name} names no blank in the text"));
        }
        let fields = names
            .into_iter()
            .map(|name| {
                let field = described.remove(&name).unwrap_or_default();
                (name, field)
            })
            .collect();
        Ok(Template {
            text: raw.text,
            description: raw.description,
            preview: raw.preview,
            fields,
        })
    }
}

/// One blank's field in the template's form.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RawField")]
pub struct Field {
    /// The question the form asks, the blank's name when absent.
    pub label: Option<String>,

    /// A dim line under the label.
    pub help: Option<String>,

    /// Whether the form may finish with the field empty.
    pub optional: bool,
    pub kind: Kind,
}

/// What a field takes, with the settings and default that belong to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One line of text.
    Line { default: String },

    /// Any number of lines of text.
    Text { default: String },

    /// One item per line, each written into the text through `item`, where
    /// `{}` stands for the line.
    List { item: String, default: Vec<String> },

    /// One of `options`, or a typed answer when `other` is set. `default`
    /// is an option's label.
    Choice {
        options: Vec<Opt>,
        other: bool,
        default: Option<String>,
    },

    /// Any number of `options`, their texts joined by `join`. `default`
    /// holds options' labels.
    Many {
        options: Vec<Opt>,
        join: String,
        default: Vec<String>,
    },

    /// Yes or no, written into the text as `on` or `off`.
    Toggle {
        on: String,
        off: String,
        default: bool,
    },
}

/// An option of a `choice` or `many` field: the label the form shows and
/// the text the prompt gets, both the same for a plain string option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opt {
    pub label: String,
    pub text: String,
}

impl TryFrom<toml::Value> for Opt {
    type Error = String;

    fn try_from(value: toml::Value) -> Result<Opt, String> {
        match value {
            toml::Value::String(text) => Ok(Opt {
                label: text.clone(),
                text,
            }),
            toml::Value::Table(mut table) => {
                let mut take = |key: &str| match table.remove(key) {
                    Some(toml::Value::String(value)) => Ok(value),
                    Some(_) => Err(format!("an option's {key} must be a string")),
                    None => Err(format!("an option table needs a {key}")),
                };
                let option = Opt {
                    label: take("label")?,
                    text: take("text")?,
                };
                match table.keys().next() {
                    Some(key) => Err(format!("an option table takes no {key}")),
                    None => Ok(option),
                }
            }
            _ => Err("an option is a string or a table with label and text".to_string()),
        }
    }
}

impl Default for Kind {
    fn default() -> Kind {
        Kind::Line {
            default: String::new(),
        }
    }
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum KindName {
    #[default]
    Line,
    Text,
    List,
    Choice,
    Many,
    Toggle,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawDefault {
    Toggle(bool),
    Text(String),
    Items(Vec<String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
    label: Option<String>,
    #[serde(default)]
    kind: KindName,
    help: Option<String>,
    default: Option<RawDefault>,
    #[serde(default)]
    optional: bool,
    item: Option<String>,
    options: Option<Vec<toml::Value>>,
    other: Option<bool>,
    join: Option<String>,
    on: Option<String>,
    off: Option<String>,
}

impl TryFrom<RawField> for Field {
    type Error = String;

    fn try_from(raw: RawField) -> Result<Field, String> {
        let name = |kind: KindName| match kind {
            KindName::Line => "line",
            KindName::Text => "text",
            KindName::List => "list",
            KindName::Choice => "choice",
            KindName::Many => "many",
            KindName::Toggle => "toggle",
        };
        let kind_name = name(raw.kind);
        let keys = [
            (
                "item",
                raw.item.is_some(),
                matches!(raw.kind, KindName::List),
            ),
            (
                "options",
                raw.options.is_some(),
                matches!(raw.kind, KindName::Choice | KindName::Many),
            ),
            (
                "other",
                raw.other.is_some(),
                matches!(raw.kind, KindName::Choice),
            ),
            (
                "join",
                raw.join.is_some(),
                matches!(raw.kind, KindName::Many),
            ),
            ("on", raw.on.is_some(), matches!(raw.kind, KindName::Toggle)),
            (
                "off",
                raw.off.is_some(),
                matches!(raw.kind, KindName::Toggle),
            ),
        ];
        if let Some((key, ..)) = keys.iter().find(|(_, given, fits)| *given && !fits) {
            return Err(format!("{key} does not apply to a {kind_name} field"));
        }
        let wrong_default = || format!("the default does not fit a {kind_name} field");
        let text_default = |default: Option<RawDefault>| match default {
            None => Ok(String::new()),
            Some(RawDefault::Text(text)) => Ok(text),
            Some(_) => Err(wrong_default()),
        };
        let items_default = |default: Option<RawDefault>| match default {
            None => Ok(Vec::new()),
            Some(RawDefault::Items(items)) => Ok(items),
            Some(RawDefault::Text(text)) => Ok(text.lines().map(str::to_string).collect()),
            Some(RawDefault::Toggle(_)) => Err(wrong_default()),
        };
        let options = |options: Option<Vec<toml::Value>>| match options {
            Some(options) if !options.is_empty() => options
                .into_iter()
                .map(Opt::try_from)
                .collect::<Result<Vec<Opt>, String>>(),
            _ => Err(format!("a {kind_name} field needs options")),
        };
        let unknown = |options: &[Opt], label: &str| {
            (!options.iter().any(|option| option.label == label))
                .then(|| format!("the default {label} names no option"))
        };
        let kind = match raw.kind {
            KindName::Line => Kind::Line {
                default: text_default(raw.default)?,
            },
            KindName::Text => Kind::Text {
                default: text_default(raw.default)?,
            },
            KindName::List => Kind::List {
                item: raw.item.unwrap_or_else(|| "{}".to_string()),
                default: items_default(raw.default)?,
            },
            KindName::Choice => {
                let options = options(raw.options)?;
                let default = match raw.default {
                    None => None,
                    Some(RawDefault::Text(text)) => Some(text),
                    Some(_) => return Err(wrong_default()),
                };
                if let Some(error) = default
                    .as_deref()
                    .and_then(|label| unknown(&options, label))
                {
                    return Err(error);
                }
                Kind::Choice {
                    options,
                    other: raw.other.unwrap_or(false),
                    default,
                }
            }
            KindName::Many => {
                let options = options(raw.options)?;
                let default = items_default(raw.default)?;
                if let Some(error) = default.iter().find_map(|label| unknown(&options, label)) {
                    return Err(error);
                }
                Kind::Many {
                    options,
                    join: raw.join.unwrap_or_else(|| ", ".to_string()),
                    default,
                }
            }
            KindName::Toggle => {
                let default = match raw.default {
                    None => false,
                    Some(RawDefault::Toggle(on)) => on,
                    Some(_) => return Err(wrong_default()),
                };
                Kind::Toggle {
                    on: raw.on.unwrap_or_else(|| "yes".to_string()),
                    off: raw.off.unwrap_or_default(),
                    default,
                }
            }
        };
        Ok(Field {
            label: raw.label,
            help: raw.help,
            optional: raw.optional,
            kind,
        })
    }
}

/// A field's answer as the form holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Typed text, or the text of the option picked from a choice.
    Text(String),

    /// The texts of the options ticked in a `many` field.
    Items(Vec<String>),
    Toggle(bool),
}

impl Field {
    /// The question the form asks for the blank `name`.
    pub fn label<'a>(&'a self, name: &'a str) -> &'a str {
        self.label.as_deref().unwrap_or(name)
    }

    /// What goes into the text in place of the blank, or `None` when the
    /// field has no answer.
    pub fn insert(&self, answer: Option<&Answer>) -> Option<String> {
        let text = match (&self.kind, answer?) {
            (Kind::List { item, .. }, Answer::Text(text)) => text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| item.replace("{}", line))
                .collect::<Vec<_>>()
                .join("\n"),
            (Kind::Many { join, .. }, Answer::Items(items)) => items.join(join),
            (Kind::Toggle { on, off, .. }, Answer::Toggle(yes)) => {
                return Some(if *yes { on.clone() } else { off.clone() });
            }
            (_, Answer::Text(text)) => text.trim().to_string(),
            _ => return None,
        };
        (!text.is_empty()).then_some(text)
    }
}

impl Template {
    /// The text with every blank filled from `answers`, or the index of the
    /// first field that needs an answer and has none, with what to say.
    pub fn fill(&self, answers: &BTreeMap<String, Answer>) -> Result<String, (usize, String)> {
        let mut values = BTreeMap::new();
        for (index, (name, field)) in self.fields.iter().enumerate() {
            match field.insert(answers.get(name)) {
                Some(value) => {
                    values.insert(name.as_str(), value);
                }
                None if field.optional => {}
                None => {
                    let message = format!("{} needs an answer", field.label(name));
                    return Err((index, message));
                }
            }
        }
        Ok(self.render(|name| values.get(name).cloned().unwrap_or_default()))
    }

    /// The text as filled so far, with each blank still without an answer
    /// left as `{name}`.
    pub fn preview(&self, answers: &BTreeMap<String, Answer>) -> String {
        self.render(|name| {
            self.fields
                .iter()
                .find(|(field_name, _)| field_name == name)
                .and_then(|(_, field)| field.insert(answers.get(name)))
                .unwrap_or_else(|| format!("{{{name}}}"))
        })
    }

    /// The text with `{{` and `}}` as single braces and each blank
    /// replaced by `value` of its name.
    fn render(&self, value: impl Fn(&str) -> String) -> String {
        let mut text = String::new();
        let mut rest = self.text.as_str();
        while let Some(ch) = rest.chars().next() {
            if let Some(after) = rest.strip_prefix("{{") {
                text.push('{');
                rest = after;
            } else if let Some(after) = rest.strip_prefix("}}") {
                text.push('}');
                rest = after;
            } else if let Some(name) = blank_at(rest) {
                text.push_str(&value(name));
                rest = &rest[name.len() + 2..];
            } else {
                text.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
        text
    }

    /// The line the command list shows: the description, or else the
    /// text's first line.
    pub fn summary(&self) -> &str {
        self.description
            .as_deref()
            .unwrap_or_else(|| self.text.lines().next().unwrap_or(""))
    }
}

/// The names of the blanks in `text`, each once, in order of first
/// appearance.
fn blanks(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("{{").or_else(|| rest.strip_prefix("}}")) {
            rest = after;
        } else if let Some(name) = blank_at(rest) {
            if !names.iter().any(|known| known == name) {
                names.push(name.to_string());
            }
            rest = &rest[name.len() + 2..];
        } else {
            rest = &rest[ch.len_utf8()..];
        }
    }
    names
}

/// The name of the blank `text` starts with, if it starts with one.
fn blank_at(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('{')?;
    let end = inner.find('}')?;
    let name = &inner[..end];
    let fits = |ch: char| ch.is_alphanumeric() || ch == '_' || ch == '-';
    (!name.is_empty() && name.chars().all(fits)).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<Template, String> {
        toml::from_str(toml).map_err(|error| error.to_string())
    }

    fn text(answer: &str) -> Answer {
        Answer::Text(answer.to_string())
    }

    const AWAY: &str = r#"
        text = "{tasks}\n\naway for {time}. {questions}"
        [fields.tasks]
        label = "what should get done?"
        kind = "list"
        item = "- {}"
        [fields.time]
        kind = "choice"
        options = ["an hour", "the night"]
        other = true
        [fields.questions]
        kind = "toggle"
        on = "ask me now"
        off = "don't wait"
    "#;

    #[test]
    fn a_template_lists_its_blanks_once_each_in_order_of_first_use() {
        let template = parse(r#"text = "{tasks} for {time}, {tasks} again""#).unwrap();
        let names: Vec<&str> = template
            .fields
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, ["tasks", "time"]);
        assert_eq!(template.fields[0].1, Field::default());
    }

    #[test]
    fn doubled_braces_are_single_braces_and_never_a_blank() {
        let template = parse(r#"text = "use {{x}} and {it} but not {a b} or {}""#).unwrap();
        assert_eq!(template.fields.len(), 1);
        let answers = BTreeMap::from([("it".to_string(), text("this"))]);
        assert_eq!(
            template.fill(&answers).unwrap(),
            "use {x} and this but not {a b} or {}"
        );
    }

    #[test]
    fn each_kind_of_field_parses_with_its_own_settings() {
        let away = parse(AWAY).unwrap();
        let kinds: Vec<&Kind> = away.fields.iter().map(|(_, field)| &field.kind).collect();
        assert_eq!(
            kinds[0],
            &Kind::List {
                item: "- {}".to_string(),
                default: Vec::new()
            }
        );
        assert!(matches!(kinds[1], Kind::Choice { other: true, .. }));
        assert!(matches!(kinds[2], Kind::Toggle { default: false, .. }));
        let many = parse(
            r#"
            text = "{langs}"
            [fields.langs]
            kind = "many"
            options = ["rust", "go"]
            default = ["go"]
            "#,
        )
        .unwrap();
        assert_eq!(
            many.fields[0].1.kind,
            Kind::Many {
                options: vec![plain("rust"), plain("go")],
                join: ", ".to_string(),
                default: vec!["go".to_string()],
            }
        );
    }

    fn plain(text: &str) -> Opt {
        Opt {
            label: text.to_string(),
            text: text.to_string(),
        }
    }

    fn options(toml: &str) -> Result<Vec<Opt>, String> {
        let template = parse(&format!(
            "text = \"{{a}}\"\n[fields.a]\nkind = \"choice\"\n{toml}"
        ))?;
        match &template.fields[0].1.kind {
            Kind::Choice { options, .. } => Ok(options.clone()),
            _ => panic!("a choice"),
        }
    }

    #[test]
    fn an_option_is_a_string_or_a_table_of_label_and_text_and_the_two_mix() {
        let options = options(
            r#"options = ["maybe", { label = "yes", text = "ask me now" }]
default = "yes""#,
        )
        .unwrap();
        let yes = Opt {
            label: "yes".to_string(),
            text: "ask me now".to_string(),
        };
        assert_eq!(options, [plain("maybe"), yes]);
    }

    #[test]
    fn an_option_table_without_label_or_text_or_with_another_key_is_an_error() {
        let error = |toml: &str| options(toml).unwrap_err();
        assert!(error(r#"options = [{ label = "yes" }]"#).contains("needs a text"));
        assert!(error(r#"options = [{ text = "x" }]"#).contains("needs a label"));
        let extra = r#"options = [{ label = "a", text = "b", colour = "red" }]"#;
        assert!(error(extra).contains("takes no colour"));
        assert!(error("options = [1]").contains("string or a table"));
    }

    #[test]
    fn a_default_names_an_option_by_its_label() {
        let tables = r#"options = [{ label = "yes", text = "y" }, { label = "no", text = "n" }]"#;
        assert!(options(&format!("{tables}\ndefault = \"no\"")).is_ok());
        let error = options(&format!("{tables}\ndefault = \"n\"")).unwrap_err();
        assert!(error.contains("the default n names no option"), "{error}");
    }

    #[test]
    fn a_send_key_is_an_error() {
        let error = parse("text = \"hi\"\nsend = true").unwrap_err();
        assert!(error.contains("send"), "{error}");
    }

    #[test]
    fn a_list_default_is_an_array_or_a_string_of_lines() {
        let items = |toml: &str| match &parse(toml).unwrap().fields[0].1.kind {
            Kind::List { default, .. } => default.clone(),
            _ => panic!("a list"),
        };
        let array = "text = \"{a}\"\n[fields.a]\nkind = \"list\"\ndefault = [\"x\", \"y\"]";
        let lines = "text = \"{a}\"\n[fields.a]\nkind = \"list\"\ndefault = \"x\\ny\"";
        assert_eq!(items(array), ["x", "y"]);
        assert_eq!(items(lines), ["x", "y"]);
    }

    #[test]
    fn a_field_with_a_wrong_setting_no_options_or_no_blank_is_an_error() {
        let error = |toml: &str| parse(toml).unwrap_err();
        let base = "text = \"{a}\"\n[fields.a]\n";
        assert!(error(&format!("{base}kind = \"choice\"")).contains("needs options"));
        assert!(error(&format!("{base}item = \"- {{}}\"")).contains("item does not apply"));
        assert!(error(&format!("{base}kind = \"toggle\"\ndefault = \"x\"")).contains("default"));
        assert!(error(&format!("{base}kind = \"box\"")).contains("box"));
        assert!(error(&format!("{base}colour = 1")).contains("colour"));
        assert!(error("text = \"{a}\"\n[fields.b]\n").contains("field b names no blank"));
    }

    #[test]
    fn each_kind_writes_its_answer_into_the_text() {
        let away = parse(AWAY).unwrap();
        let answers = BTreeMap::from([
            ("tasks".to_string(), text("fix the bar\n\n write notes ")),
            ("time".to_string(), text("an hour")),
            ("questions".to_string(), Answer::Toggle(false)),
        ]);
        assert_eq!(
            away.fill(&answers).unwrap(),
            "- fix the bar\n- write notes\n\naway for an hour. don't wait"
        );
        let many = Field {
            kind: Kind::Many {
                options: Vec::new(),
                join: " and ".to_string(),
                default: Vec::new(),
            },
            ..Field::default()
        };
        let both = Answer::Items(vec!["rust".to_string(), "go".to_string()]);
        assert_eq!(many.insert(Some(&both)).as_deref(), Some("rust and go"));
    }

    #[test]
    fn a_required_field_without_an_answer_holds_the_fill_back_and_an_optional_one_writes_nothing() {
        let mut away = parse(AWAY).unwrap();
        let answers = BTreeMap::from([("time".to_string(), text("an hour"))]);
        assert_eq!(
            away.fill(&answers),
            Err((0, "what should get done? needs an answer".to_string()))
        );
        away.fields[0].1.optional = true;
        away.fields[2].1.optional = true;
        assert_eq!(away.fill(&answers).unwrap(), "\n\naway for an hour. ");
    }

    #[test]
    fn the_preview_keeps_each_unanswered_blank_as_written() {
        let away = parse(AWAY).unwrap();
        let answers = BTreeMap::from([("time".to_string(), text("the night"))]);
        assert_eq!(
            away.preview(&answers),
            "{tasks}\n\naway for the night. {questions}"
        );
    }

    #[test]
    fn the_summary_is_the_description_or_else_the_first_line() {
        let mut away = parse(r#"text = "{tasks}\n\nI'm away""#).unwrap();
        assert_eq!(away.summary(), "{tasks}");
        away.description = Some("before I step out".to_string());
        assert_eq!(away.summary(), "before I step out");
    }
}
