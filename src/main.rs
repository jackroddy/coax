use std::path::{Path, PathBuf};
use std::time::Instant;

use agent_client_protocol::v1::schema::SessionId;
use agent_client_protocol::{AcpAgent, Error};
use coax::agents::{self, Agents};
use coax::app::{Opening, Start};
use coax::config::Config;
use coax::links::LinksConfig;
use coax::log;
use coax::picker::Choice;
use coax::playback::Player;
use coax::scenario::Scenario;
use coax::{acp, app, picker, report};
use serde_json::json;
use tokio::task::LocalSet;

const USAGE: &str = "usage: coax [--prompt <text>] [--allow] \
     [--agent <name> [--session <id>] | <agent command> [args...]]";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    first_prompt: Option<String>,
    auto_allow: bool,
    launch: Launch,
    session: Option<String>,
    input: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Launch {
    Command(Vec<String>),
    Named(String),
    Pick,
}

fn parse(mut args: Vec<String>) -> Option<Options> {
    let mut first_prompt = None;
    let mut auto_allow = false;
    let mut agent = None;
    let mut session = None;
    #[cfg_attr(not(feature = "fake-agent"), allow(unused_mut))]
    let mut input = None;
    loop {
        match args.first().map(String::as_str) {
            Some("--prompt") if args.len() > 1 => {
                first_prompt = Some(args[1].clone());
                args.drain(..2);
            }
            Some("--agent") if args.len() > 1 => {
                agent = Some(args[1].clone());
                args.drain(..2);
            }
            Some("--session") if args.len() > 1 => {
                session = Some(args[1].clone());
                args.drain(..2);
            }
            Some("--allow") => {
                auto_allow = true;
                args.remove(0);
            }
            #[cfg(feature = "fake-agent")]
            Some("--input") if args.len() > 1 => {
                input = Some(args[1].clone());
                args.drain(..2);
            }
            _ => break,
        }
    }
    let launch = match (agent, args.first()) {
        (_, Some(word)) if word.starts_with('-') => return None,
        (Some(_), Some(_)) => return None,
        (Some(name), None) => Launch::Named(name),
        (None, Some(_)) => Launch::Command(args),
        (None, None) => Launch::Pick,
    };
    if session.is_some() && !matches!(launch, Launch::Named(_)) {
        return None;
    }
    Some(Options {
        first_prompt,
        auto_allow,
        launch,
        session,
        input,
    })
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn load_config() -> (Config, PathBuf) {
    let Some(path) = Config::path() else {
        fail(2, "neither XDG_CONFIG_HOME nor HOME is set");
    };
    match Config::load(&path) {
        Ok(config) => {
            coax::log::info("config", json!({"path": path}));
            coax::palette::init(config.palette.resolve(coax::palette::query_terminal()));
            coax::glyphs::init(coax::glyphs::detect(config.glyphs, env_var));
            (config, path)
        }
        Err(error) => fail(2, &error),
    }
}

/// The launch arguments, with the text after `--prompt` left out: a prompt
/// is logged only as the message that sends it.
fn logged_args(mut args: Vec<String>) -> Vec<String> {
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--prompt" if at + 1 < args.len() => {
                args[at + 1] = "<prompt>".to_string();
                at += 2;
            }
            "--agent" | "--session" | "--input" => at += 2,
            "--allow" => at += 1,
            _ => break,
        }
    }
    args
}

/// The keys a scenario file's `input` types in.
fn player(path: Option<&str>) -> Player {
    let Some(path) = path else {
        return Player::default();
    };
    let json =
        std::fs::read_to_string(path).unwrap_or_else(|error| fail(2, &format!("{path}: {error}")));
    let scenario =
        Scenario::parse(&json).unwrap_or_else(|error| fail(2, &format!("{path}: {error}")));
    Player::new(scenario.input).unwrap_or_else(|error| fail(2, &format!("{path}: {error}")))
}

fn fail(code: i32, message: &str) -> ! {
    coax::log::info("error", json!({"text": message}));
    eprintln!("coax: {message}");
    std::process::exit(code);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    coax::log::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    coax::log::info(
        "launch",
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "args": logged_args(args.clone()),
            "cwd": std::env::current_dir().ok(),
        }),
    );
    let resume_args = report::resume_args(&args);
    let Some(options) = parse(args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let start = match options.session {
        Some(id) => Start::Load(SessionId::new(id), None),
        None => Start::New,
    };
    let mut opening = Opening {
        start,
        first_prompt: options.first_prompt,
        auto_allow: options.auto_allow,
        keys: app::key_thread(),
        player: player(options.input.as_deref()),
        links: LinksConfig::default(),
        templates: Default::default(),
        registry: None,
        resume_args,
    };
    let result = match options.launch {
        Launch::Command(words) => match AcpAgent::from_args(words.clone()) {
            Ok(agent) => {
                coax::glyphs::init(coax::glyphs::detect(None, env_var));
                log::info("agent.chosen", json!({"command": words}));
                open(agent, None, opening).await
            }
            Err(error) => fail(2, &error.to_string()),
        },
        Launch::Named(name) => {
            let (config, path) = load_config();
            match config.agents.get(&name) {
                Some(agent) => {
                    log::info("agent.chosen", json!({"name": name}));
                    opening.links = config.links.clone();
                    opening.templates = config.templates.clone();
                    opening.registry = config.registries.get(&name).cloned();
                    open(AcpAgent::new(agent.clone()), Some(name), opening).await
                }
                None => fail(2, &format!("no agent named {name} in {}", path.display())),
            }
        }
        Launch::Pick => {
            let (config, path) = load_config();
            LocalSet::new()
                .run_until(pick(&config, &path, opening))
                .await
        }
    };
    if let Err(error) = result {
        fail(1, &acp::describe(&error));
    }
}

/// Run `agent` on `opening`. A session to load that the agent does not list
/// for the folder is an error before the screen opens.
async fn open(agent: AcpAgent, name: Option<String>, mut opening: Opening) -> Result<(), Error> {
    let command = agent.config().command().display().to_string();
    let agent = log::tap(agent, name.as_deref().unwrap_or(&command));
    acp::run(agent, &command, async |link| {
        let began = Instant::now();
        let init = link.initialize().await?;
        log::info(
            "agent.initialized",
            json!({"ms": began.elapsed().as_millis() as u64}),
        );
        if let Start::Load(id, title) = &mut opening.start {
            let cwd = std::env::current_dir().map_err(Error::into_internal_error)?;
            *title = agents::listed_title(&link, &init, id, &cwd).await?;
        }
        app::run(link, init, name, opening).await
    })
    .await
}

/// Start every configured agent, show the picker over their sessions and
/// open the one chosen.
async fn pick(config: &Config, path: &Path, mut opening: Opening) -> Result<(), Error> {
    opening.links = config.links.clone();
    opening.templates = config.templates.clone();
    let cwd = std::env::current_dir().map_err(Error::into_internal_error)?;
    let mut agents = Agents::start(config, &cwd);
    let choice = picker::pick(
        config,
        path,
        &mut agents.listings,
        &mut opening.keys,
        &mut opening.player,
    )
    .await
    .map_err(Error::into_internal_error)?;
    match &choice {
        Choice::Quit => log::info("picker", json!({"choice": "quit"})),
        Choice::Agent(name) => log::info("picker", json!({"choice": "new", "agent": name})),
        Choice::Session { agent, id, .. } => log::info(
            "picker",
            json!({"choice": "load", "agent": agent, "session": id.0}),
        ),
    }
    match choice {
        Choice::Quit => Ok(()),
        Choice::Agent(name) => {
            opening.registry = config.registries.get(name).cloned();
            agents.open(name, opening).await
        }
        Choice::Session { agent, id, title } => {
            opening.registry = config.registries.get(agent).cloned();
            opening.start = Start::Load(id, title);
            agents.open(agent, opening).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_first_word_that_is_not_a_flag_starts_a_raw_command() {
        let options = parse(args("--allow coax-fake-agent scenarios/hello.json")).unwrap();
        assert!(options.auto_allow);
        assert_eq!(
            options.launch,
            Launch::Command(args("coax-fake-agent scenarios/hello.json"))
        );
    }

    #[test]
    fn agent_names_an_entry_in_the_config() {
        let options = parse(args("--agent claude --prompt hi")).unwrap();
        assert_eq!(options.launch, Launch::Named("claude".to_string()));
        assert_eq!(options.first_prompt.as_deref(), Some("hi"));
    }

    #[test]
    fn flags_with_no_agent_open_the_picker() {
        let options = parse(args("--prompt hi")).unwrap();
        assert_eq!(options.launch, Launch::Pick);
        assert_eq!(parse(Vec::new()).unwrap().launch, Launch::Pick);
    }

    #[test]
    fn agent_and_a_raw_command_together_are_a_usage_error() {
        assert_eq!(parse(args("--agent claude coax-fake-agent")), None);
    }

    #[test]
    fn session_with_agent_names_the_session_to_load() {
        let options = parse(args("--agent claude --allow --session 7f3a")).unwrap();
        assert_eq!(options.launch, Launch::Named("claude".to_string()));
        assert_eq!(options.session.as_deref(), Some("7f3a"));
        assert!(options.auto_allow);
        assert_eq!(parse(args("--agent claude")).unwrap().session, None);
    }

    #[test]
    fn session_without_agent_is_a_usage_error() {
        assert_eq!(parse(args("--session 7f3a")), None);
        assert_eq!(parse(args("--session 7f3a coax-fake-agent")), None);
        assert_eq!(parse(args("--agent claude --session")), None);
    }

    #[test]
    fn the_logged_args_hide_the_prompt_and_keep_the_session() {
        assert_eq!(
            logged_args(args("--prompt hi --agent claude --session 7f3a --allow")),
            args("--prompt <prompt> --agent claude --session 7f3a --allow")
        );
    }

    #[test]
    fn an_unknown_flag_is_a_usage_error() {
        assert_eq!(parse(args("--verbose")), None);
        assert_eq!(parse(args("--agent")), None);
    }
}
