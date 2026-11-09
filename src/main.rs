use std::path::{Path, PathBuf};
use std::time::Instant;

use agent_client_protocol::{AcpAgent, Error};
use coax::agents::Agents;
use coax::app::{Opening, Start};
use coax::config::Config;
use coax::log;
use coax::picker::Choice;
use coax::{acp, app, picker};
use serde_json::json;
use tokio::task::LocalSet;

const USAGE: &str =
    "usage: coax [--prompt <text>] [--allow] [--agent <name> | <agent command> [args...]]";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    first_prompt: Option<String>,
    auto_allow: bool,
    launch: Launch,
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
            Some("--allow") => {
                auto_allow = true;
                args.remove(0);
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
    Some(Options {
        first_prompt,
        auto_allow,
        launch,
    })
}

fn load_config() -> (Config, PathBuf) {
    let Some(path) = Config::path() else {
        fail(2, "neither XDG_CONFIG_HOME nor HOME is set");
    };
    match Config::load(&path) {
        Ok(config) => {
            coax::log::info("config", json!({"path": path}));
            coax::palette::init(config.palette.resolve());
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
            "--agent" => at += 2,
            "--allow" => at += 1,
            _ => break,
        }
    }
    args
}

fn fail(code: i32, message: &str) -> ! {
    coax::log::info("error", json!({"text": message}));
    eprintln!("coax: {message}");
    std::process::exit(code);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    coax::log::init();
    coax::log::info(
        "launch",
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "args": logged_args(std::env::args().skip(1).collect()),
            "cwd": std::env::current_dir().ok(),
        }),
    );
    let Some(options) = parse(std::env::args().skip(1).collect()) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let opening = Opening {
        start: Start::New,
        first_prompt: options.first_prompt,
        auto_allow: options.auto_allow,
        keys: app::key_thread(),
    };
    let result = match options.launch {
        Launch::Command(words) => match AcpAgent::from_args(words.clone()) {
            Ok(agent) => {
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

async fn open(agent: AcpAgent, name: Option<String>, opening: Opening) -> Result<(), Error> {
    let command = agent.config().command().display().to_string();
    let agent = log::tap(agent, name.as_deref().unwrap_or(&command));
    acp::run(agent, &command, async |link| {
        let began = Instant::now();
        let init = link.initialize().await?;
        log::info(
            "agent.initialized",
            json!({"ms": began.elapsed().as_millis() as u64}),
        );
        app::run(link, init, name, opening).await
    })
    .await
}

/// Start every configured agent, show the picker over their sessions and
/// open the one chosen.
async fn pick(config: &Config, path: &Path, mut opening: Opening) -> Result<(), Error> {
    let cwd = std::env::current_dir().map_err(Error::into_internal_error)?;
    let mut agents = Agents::start(config, &cwd);
    let choice = picker::pick(config, path, &mut agents.listings, &mut opening.keys)
        .await
        .map_err(Error::into_internal_error)?;
    match &choice {
        Choice::Quit => log::info("picker", json!({"choice": "quit"})),
        Choice::Agent(name) => log::info("picker", json!({"choice": "new", "agent": name})),
        Choice::Session { agent, id } => log::info(
            "picker",
            json!({"choice": "load", "agent": agent, "session": id.0}),
        ),
    }
    match choice {
        Choice::Quit => Ok(()),
        Choice::Agent(name) => agents.open(name, opening).await,
        Choice::Session { agent, id } => {
            opening.start = Start::Load(id);
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
    fn an_unknown_flag_is_a_usage_error() {
        assert_eq!(parse(args("--verbose")), None);
        assert_eq!(parse(args("--agent")), None);
    }
}
