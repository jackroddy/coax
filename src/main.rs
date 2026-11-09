use std::path::PathBuf;

use agent_client_protocol::AcpAgent;
use coax::config::Config;
use coax::{acp, app, picker};

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
        Ok(config) => (config, path),
        Err(error) => fail(2, &error),
    }
}

fn fail(code: i32, message: &str) -> ! {
    eprintln!("coax: {message}");
    std::process::exit(code);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let Some(options) = parse(std::env::args().skip(1).collect()) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let (agent, name) = match options.launch {
        Launch::Command(words) => match AcpAgent::from_args(words) {
            Ok(agent) => (agent, None),
            Err(error) => fail(2, &error.to_string()),
        },
        Launch::Named(name) => {
            let (config, path) = load_config();
            match config.agents.get(&name) {
                Some(agent) => (AcpAgent::new(agent.clone()), Some(name)),
                None => fail(2, &format!("no agent named {name} in {}", path.display())),
            }
        }
        Launch::Pick => {
            let (config, path) = load_config();
            match picker::pick(&config, &path) {
                Ok(Some(name)) => (AcpAgent::new(config.agents[&name].clone()), Some(name)),
                Ok(None) => return,
                Err(error) => fail(1, &error.to_string()),
            }
        }
    };
    let first_prompt = options.first_prompt;
    let auto_allow = options.auto_allow;
    if let Err(error) = acp::run(agent, async |link| {
        app::run(link, name, first_prompt, auto_allow).await
    })
    .await
    {
        ratatui::restore();
        fail(1, &error.to_string());
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
