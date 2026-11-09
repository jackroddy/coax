use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use agent_client_protocol::AcpAgentConfig;
use serde::Deserialize;

/// The settings read from `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The agent the picker starts on.
    pub default: Option<String>,

    /// The agents to choose from, by name.
    #[serde(default)]
    pub agents: BTreeMap<String, AcpAgentConfig>,
}

impl Config {
    /// The path of `config.toml`, under `$XDG_CONFIG_HOME` or `~/.config`.
    pub fn path() -> Option<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(base.join("coax").join("config.toml"))
    }

    /// Read the config at `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Config::default()),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_entry_reads_its_command_args_and_env() {
        let config: Config = toml::from_str(
            r#"
            default = "claude"

            [agents.claude]
            command = "mise"
            args = ["exec", "node@22.23.4", "--", "claude-agent-acp"]
            env = { CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION = "false" }

            [agents.fake]
            command = "coax-fake-agent"
            "#,
        )
        .unwrap();
        assert_eq!(config.default.as_deref(), Some("claude"));
        let expected = AcpAgentConfig::new("mise")
            .args(["exec", "node@22.23.4", "--", "claude-agent-acp"])
            .env("CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION", "false");
        assert_eq!(config.agents["claude"], expected);
        assert_eq!(
            config.agents["fake"],
            AcpAgentConfig::new("coax-fake-agent")
        );
    }

    #[test]
    fn a_missing_file_loads_as_a_config_with_no_agents() {
        let config = Config::load(Path::new("/nonexistent/coax/config.toml")).unwrap();
        assert!(config.agents.is_empty());
        assert!(config.default.is_none());
    }

    #[test]
    fn an_unknown_key_is_an_error_naming_the_file() {
        let path = std::env::temp_dir().join("coax-config-unknown-key.toml");
        std::fs::write(&path, "agent = \"claude\"\n").unwrap();
        let error = Config::load(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert!(error.contains("coax-config-unknown-key.toml"), "{error}");
    }
}
