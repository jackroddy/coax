use agent_client_protocol::v1::schema::{
    AvailableCommand, Cost, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOption, SessionConfigSelectOptions, SessionModeState, SessionUpdate, Usage,
};

/// What the agent reports about the session besides the conversation: its
/// settings, how full its context is, and what it has used.
#[derive(Default)]
pub struct Settings {
    options: Vec<SessionConfigOption>,

    /// The older modes API, shown as a mode setting when the agent sends no
    /// config options.
    modes: Option<SessionModeState>,
    pub context: Option<Context>,
    pub cost: Option<Cost>,
    pub tokens: Option<Tokens>,

    /// The slash commands the agent offers.
    pub commands: Vec<AvailableCommand>,
}

pub struct Context {
    pub used: u64,
    pub size: u64,
}

/// Session totals: `up` is input plus cached reads and writes, `down` is output.
pub struct Tokens {
    pub up: u64,
    pub down: u64,
}

/// One setting as the header shows it.
pub struct Setting {
    /// The config option's id, or `mode` for the modes API.
    pub id: String,
    pub name: String,

    /// The text of the setting on the status line and in the overflow list.
    pub label: String,
    pub description: Option<String>,
    pub control: Control,

    /// Whether the setting is the agent's mode, and the mode is one in which
    /// the agent does not ask.
    pub unguarded: bool,

    /// Whether a change goes through `session/set_mode`.
    pub from_modes: bool,
    pub category: Category,
}

/// What a setting sets, from the config option's category, in header order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Mode,
    Model,
    ThoughtLevel,
    ModelConfig,
    Other,
}

impl Category {
    fn of(category: Option<&SessionConfigOptionCategory>) -> Category {
        match category {
            Some(SessionConfigOptionCategory::Mode) => Category::Mode,
            Some(SessionConfigOptionCategory::Model) => Category::Model,
            Some(SessionConfigOptionCategory::ThoughtLevel) => Category::ThoughtLevel,
            Some(SessionConfigOptionCategory::ModelConfig) => Category::ModelConfig,
            _ => Category::Other,
        }
    }

    /// Whether the setting is model config or has no category.
    pub fn rarely_changed(self) -> bool {
        self > Category::ThoughtLevel
    }
}

pub enum Control {
    Choose(Vec<Choice>),
    Flip(bool),
}

pub struct Choice {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
    pub current: bool,
}

impl Settings {
    /// Take the settings a `session/new` or `session/load` response carries.
    pub fn open(
        &mut self,
        options: Option<Vec<SessionConfigOption>>,
        modes: Option<SessionModeState>,
    ) {
        self.options = options.unwrap_or_default();
        self.modes = modes;
    }

    pub fn set_options(&mut self, options: Vec<SessionConfigOption>) {
        self.options = options;
    }

    pub fn set_mode(&mut self, id: &str) {
        if let Some(modes) = &mut self.modes {
            modes.current_mode_id = id.to_string().into();
        }
    }

    pub fn apply(&mut self, update: &SessionUpdate) {
        match update {
            SessionUpdate::ConfigOptionUpdate(update) => {
                self.options = update.config_options.clone();
            }
            SessionUpdate::CurrentModeUpdate(update) => {
                self.set_mode(&update.current_mode_id.0);
            }
            SessionUpdate::AvailableCommandsUpdate(update) => {
                self.commands = update.available_commands.clone();
            }
            SessionUpdate::UsageUpdate(update) => {
                self.context = Some(Context {
                    used: update.used,
                    size: update.size,
                });
                if update.cost.is_some() {
                    self.cost = update.cost.clone();
                }
            }
            _ => {}
        }
    }

    /// Add the usage a prompt response reported to the session's totals.
    /// The Claude adapter reports one turn's tokens in each response.
    pub fn used(&mut self, usage: &Usage) {
        let (up, down) = match &self.tokens {
            Some(tokens) => (tokens.up, tokens.down),
            None => (0, 0),
        };
        self.tokens = Some(Tokens {
            up: up
                + usage.input_tokens
                + usage.cached_read_tokens.unwrap_or(0)
                + usage.cached_write_tokens.unwrap_or(0),
            down: down + usage.output_tokens,
        });
    }

    /// The model setting's current value as the header shows it.
    pub fn model(&self) -> Option<String> {
        self.settings()
            .into_iter()
            .find(|setting| setting.category == Category::Model)
            .map(|setting| setting.label)
    }

    /// The settings in header order: mode, model, thought level, model
    /// config, then the rest in the order the agent sent them.
    pub fn settings(&self) -> Vec<Setting> {
        let mut settings: Vec<Setting> = self.options.iter().filter_map(setting).collect();
        if self.options.is_empty()
            && let Some(modes) = &self.modes
        {
            settings.push(mode_setting(modes));
        }
        settings.sort_by_key(|setting| setting.category);
        settings
    }
}

/// The modes in which the agent does not ask before it acts.
fn is_unguarded(mode: &str) -> bool {
    matches!(mode, "auto" | "bypassPermissions")
}

fn setting(option: &SessionConfigOption) -> Option<Setting> {
    let category = Category::of(option.category.as_ref());
    let value_only = matches!(category, Category::Mode | Category::Model);
    let (value, control, current) = match &option.kind {
        SessionConfigKind::Select(select) => {
            let current = select.current_value.0.to_string();
            let choices: Vec<Choice> = flatten(&select.options)
                .map(|choice| Choice {
                    current: *choice.value.0 == *current,
                    value: choice.value.0.to_string(),
                    name: choice.name.clone(),
                    description: choice.description.clone(),
                })
                .collect();
            let shown = choices
                .iter()
                .find(|choice| choice.current)
                .map_or(current.clone(), |choice| choice.name.clone());
            (shown, Control::Choose(choices), current)
        }
        SessionConfigKind::Boolean(boolean) => {
            let shown = if boolean.current_value { "on" } else { "off" };
            (
                shown.to_string(),
                Control::Flip(boolean.current_value),
                String::new(),
            )
        }
        _ => return None,
    };
    let label = if value_only {
        value
    } else {
        format!("{} {value}", option.name)
    };
    Some(Setting {
        id: option.id.0.to_string(),
        name: option.name.clone(),
        label,
        description: option.description.clone(),
        control,
        unguarded: category == Category::Mode && is_unguarded(&current),
        from_modes: false,
        category,
    })
}

fn mode_setting(modes: &SessionModeState) -> Setting {
    let current = modes.current_mode_id.0.to_string();
    let choices: Vec<Choice> = modes
        .available_modes
        .iter()
        .map(|mode| Choice {
            value: mode.id.0.to_string(),
            name: mode.name.clone(),
            description: mode.description.clone(),
            current: *mode.id.0 == *current,
        })
        .collect();
    let label = choices
        .iter()
        .find(|choice| choice.current)
        .map_or(current.clone(), |choice| choice.name.clone());
    Setting {
        id: "mode".to_string(),
        name: "mode".to_string(),
        label,
        description: None,
        control: Control::Choose(choices),
        unguarded: is_unguarded(&current),
        from_modes: true,
        category: Category::Mode,
    }
}

fn flatten(
    options: &SessionConfigSelectOptions,
) -> impl Iterator<Item = &SessionConfigSelectOption> {
    let (ungrouped, grouped) = match options {
        SessionConfigSelectOptions::Ungrouped(options) => (Some(options), None),
        SessionConfigSelectOptions::Grouped(groups) => (None, Some(groups)),
        _ => (None, None),
    };
    ungrouped.into_iter().flatten().chain(
        grouped
            .into_iter()
            .flatten()
            .flat_map(|group| &group.options),
    )
}

/// The warning shown before the model changes in a conversation: the new
/// model has to read it again, and its cache is empty.
pub fn rereads(model: &str, used: Option<u64>) -> String {
    let size = used.map_or(String::new(), |used| format!(" ({} tokens)", count(used)));
    format!("rereads the conversation{size} through {model} on the next turn, without the cache")
}

/// A count of tokens as `4`, `28k` or `1.0M`.
pub fn count(tokens: u64) -> String {
    match tokens {
        ..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{}k", tokens / 1_000),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

/// A cost as `$0.14`, or the amount and currency when it is not dollars.
pub fn money(cost: &Cost) -> String {
    if cost.currency == "USD" {
        format!("${:.2}", cost.amount)
    } else {
        format!("{:.2} {}", cost.amount, cost.currency)
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{SessionMode, SessionUpdate, UsageUpdate};

    use super::*;

    #[test]
    fn counts_read_as_plain_numbers_then_thousands_then_millions() {
        assert_eq!(count(4), "4");
        assert_eq!(count(999), "999");
        assert_eq!(count(28_481), "28k");
        assert_eq!(count(200_000), "200k");
        assert_eq!(count(1_000_000), "1.0M");
        assert_eq!(count(2_450_000), "2.5M");
    }

    #[test]
    fn the_rereading_warning_names_the_model_and_the_context_when_it_is_known() {
        assert_eq!(
            rereads("Sonnet", Some(412_000)),
            "rereads the conversation (412k tokens) through Sonnet on the next turn, without the cache"
        );
        assert_eq!(
            rereads("Sonnet", None),
            "rereads the conversation through Sonnet on the next turn, without the cache"
        );
    }

    #[test]
    fn a_cost_in_dollars_reads_with_a_dollar_sign() {
        assert_eq!(money(&Cost::new(0.136, "USD")), "$0.14");
        assert_eq!(money(&Cost::new(2.0, "EUR")), "2.00 EUR");
    }

    #[test]
    fn a_usage_update_sets_the_context_and_keeps_the_cost_it_last_carried() {
        let mut settings = Settings::default();
        let with_cost = UsageUpdate::new(10, 100).cost(Cost::new(0.5, "USD"));
        settings.apply(&SessionUpdate::UsageUpdate(with_cost));
        settings.apply(&SessionUpdate::UsageUpdate(UsageUpdate::new(20, 100)));
        assert_eq!(settings.context.unwrap().used, 20);
        assert_eq!(settings.cost.unwrap().amount, 0.5);
    }

    #[test]
    fn the_modes_api_shows_as_a_mode_setting_when_there_are_no_config_options() {
        let mut settings = Settings::default();
        let modes = SessionModeState::new(
            "auto",
            vec![
                SessionMode::new("plan", "Plan"),
                SessionMode::new("auto", "Auto"),
            ],
        );
        settings.open(None, Some(modes));
        let shown = settings.settings();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].label, "Auto");
        assert!(shown[0].from_modes);
        assert!(shown[0].unguarded);
        settings.set_mode("plan");
        let shown = settings.settings();
        assert_eq!(shown[0].label, "Plan");
        assert!(!shown[0].unguarded);
    }

    #[test]
    fn each_turns_usage_adds_to_the_session_totals_and_the_cost_shows_the_latest() {
        let mut settings = Settings::default();
        let mut usage = Usage::new(110, 30, 10);
        usage.input_tokens = 10;
        usage.output_tokens = 30;
        usage.cached_read_tokens = Some(70);
        usage.cached_write_tokens = Some(30);
        settings.used(&usage);
        settings.used(&usage);
        let tokens = settings.tokens.as_ref().unwrap();
        assert_eq!((tokens.up, tokens.down), (220, 60));

        for amount in [0.10, 0.25] {
            settings.apply(&SessionUpdate::UsageUpdate(
                UsageUpdate::new(1, 2).cost(Cost::new(amount, "USD")),
            ));
        }
        assert_eq!(settings.cost.as_ref().unwrap().amount, 0.25);
    }
}
