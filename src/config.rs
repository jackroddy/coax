use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use agent_client_protocol::AcpAgentConfig;
use serde::Deserialize;

use crate::links::LinksConfig;
use crate::palette::PaletteConfig;

/// The settings read from `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The agent the picker starts on.
    pub default: Option<String>,

    /// The agents to choose from, by name.
    #[serde(default)]
    pub agents: BTreeMap<String, AcpAgentConfig>,

    /// Colour overrides.
    #[serde(default)]
    pub palette: PaletteConfig,

    /// What the links in agent text do.
    #[serde(default)]
    pub links: LinksConfig,
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
    use ratatui::style::Color;

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
    fn a_links_table_sets_the_pane_the_urls_and_the_scheme_commands() {
        let config: Config = toml::from_str(
            "[links]\nopen_urls = true\n[links.schemes.foam]\nhover = [\"foam\", \"show\", \"{target}\"]\n",
        )
        .unwrap();
        assert!(config.links.open_urls);
        assert_eq!(config.links.decade_pane, "code");
        assert_eq!(config.links.schemes["foam"].hover[2], "{target}");
        assert!(!Config::default().links.open_urls);
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

    #[test]
    fn a_palette_table_overrides_the_named_colours() {
        let config: Config = toml::from_str(
            "[palette]\naccent = \"#DA745B\"\ncyan = \"darkgray\"\nmuted = \"#6D6977\"\n",
        )
        .unwrap();
        let palette = config.palette.resolve();
        assert_eq!(palette.accent, Color::Rgb(0xDA, 0x74, 0x5B));
        assert_eq!(palette.cyan, Color::DarkGray);
        assert_eq!(palette.muted, Some(Color::Rgb(0x6D, 0x69, 0x77)));
        assert_eq!(palette.green, Color::Green);
    }

    #[test]
    fn a_bad_palette_value_is_an_error_naming_the_file() {
        let path = std::env::temp_dir().join("coax-config-bad-colour.toml");
        std::fs::write(&path, "[palette]\naccent = \"salmon\"\n").unwrap();
        let error = Config::load(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert!(error.contains("coax-config-bad-colour.toml"), "{error}");
        assert!(error.contains("salmon"), "{error}");
    }

    #[test]
    fn an_unknown_palette_name_is_an_error() {
        assert!(toml::from_str::<Config>("[palette]\nbrown = \"#8E5C4C\"\n").is_err());
    }

    #[test]
    fn an_absent_palette_gives_the_ansi_fallbacks() {
        let palette = Config::default().palette.resolve();
        assert_eq!(palette.accent, Color::Yellow);
        assert_eq!(palette.muted, None);
        assert_eq!(palette.selection, Color::DarkGray);
        assert_eq!(palette.foreground, Color::Reset);
        assert_eq!(palette.orange, Color::LightRed);
        assert_eq!(palette.blue, Color::Blue);
    }
}
