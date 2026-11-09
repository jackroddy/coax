use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use decade_channel::Show;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

/// How long a hover command runs before it is killed.
pub const HOVER_TIMEOUT: Duration = Duration::from_secs(2);

/// The most lines of a command's output a popup shows.
pub const POPUP_LINES: usize = 15;

/// The `[links]` table of `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LinksConfig {
    /// The decade pane that shows files.
    pub decade_pane: String,

    /// Whether a click on an `http` or `https` link runs `xdg-open`.
    pub open_urls: bool,

    /// The commands for the other schemes, by scheme.
    pub schemes: BTreeMap<String, SchemeConfig>,
}

impl Default for LinksConfig {
    fn default() -> LinksConfig {
        LinksConfig {
            decade_pane: "code".to_string(),
            open_urls: false,
            schemes: BTreeMap::new(),
        }
    }
}

/// The commands of one link scheme. `{target}` in an argument stands for the
/// part of the link after the scheme.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemeConfig {
    /// Run on hover; its output fills the popup.
    pub hover: Vec<String>,

    /// Run detached on click.
    pub click: Option<Vec<String>>,
}

/// What a link's target asks for, by its scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind<'a> {
    /// Text drawn in the palette colour of this name.
    Color(&'a str),
    File {
        path: &'a str,
        line: Option<u32>,
    },
    Url,
    Scheme {
        scheme: &'a str,
        rest: &'a str,
    },

    /// A target with no scheme.
    Inert,
}

pub fn classify(target: &str) -> Kind<'_> {
    let Some((scheme, rest)) = target.split_once(':') else {
        return Kind::Inert;
    };
    let word = scheme.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "+.-".contains(ch));
    if !word {
        return Kind::Inert;
    }
    match scheme.to_ascii_lowercase().as_str() {
        "color" => Kind::Color(rest),
        "file" => file(rest),
        "http" | "https" => Kind::Url,
        _ => Kind::Scheme { scheme, rest },
    }
}

fn file(rest: &str) -> Kind<'_> {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (path, line) = match rest.rsplit_once('#') {
        Some((path, at)) => (path, at.strip_prefix('L').unwrap_or(at).parse().ok()),
        None => (rest, None),
    };
    if path.is_empty() {
        Kind::Inert
    } else {
        Kind::File { path, line }
    }
}

/// The argument list `argv` with `{target}` replaced by `rest`. A `rest` that
/// is empty or starts with `-` gives `None`, so a link cannot pass a flag.
fn substitute(argv: &[String], rest: &str) -> Option<Vec<String>> {
    if argv.is_empty() || rest.is_empty() || rest.starts_with('-') {
        return None;
    }
    Some(
        argv.iter()
            .map(|arg| arg.replace("{target}", rest))
            .collect(),
    )
}

fn scheme_config<'a>(scheme: &str, config: &'a LinksConfig) -> Option<&'a SchemeConfig> {
    config.schemes.get(&scheme.to_ascii_lowercase())
}

/// The command a hover over `target` runs, with its arguments filled in.
pub fn hover_command(target: &str, config: &LinksConfig) -> Option<Vec<String>> {
    let Kind::Scheme { scheme, rest } = classify(target) else {
        return None;
    };
    substitute(&scheme_config(scheme, config)?.hover, rest)
}

/// What a click on a link does.
#[derive(Debug, Clone, PartialEq)]
pub enum Click {
    /// Send `show` to the decade pane `to`.
    Show { to: String, show: Show },

    /// Run this command detached.
    Run(Vec<String>),
}

pub fn click(target: &str, config: &LinksConfig, cwd: &Path) -> Option<Click> {
    match classify(target) {
        Kind::File { path, line } => Some(Click::Show {
            to: config.decade_pane.clone(),
            show: Show {
                kind: "file".to_string(),
                target: cwd.join(path).to_string_lossy().into_owned(),
                at: line.map(|line| json!(line)),
            },
        }),
        Kind::Url if config.open_urls => {
            Some(Click::Run(vec!["xdg-open".to_string(), target.to_string()]))
        }
        Kind::Scheme { scheme, rest } => {
            let argv = scheme_config(scheme, config)?.click.as_ref()?;
            substitute(argv, rest).map(Click::Run)
        }
        _ => None,
    }
}

/// What a hover command gave, kept per target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    Loading,
    Text(String),
    Failed(String),
}

/// The lines a popup over `target` shows, or `None` when it shows nothing.
pub fn popup(
    target: &str,
    title: Option<&str>,
    config: &LinksConfig,
    cwd: &Path,
    cache: &HashMap<String, Loaded>,
) -> Option<Vec<String>> {
    let mut lines = match classify(target) {
        Kind::File { path, line } => {
            let full = cwd.join(path);
            let shown = full.strip_prefix(cwd).unwrap_or(&full).display();
            vec![match line {
                Some(line) => format!("{shown}:{line}"),
                None => shown.to_string(),
            }]
        }
        Kind::Url => vec![target.to_string()],
        Kind::Scheme { .. } => {
            hover_command(target, config)?;
            return Some(match cache.get(target) {
                None | Some(Loaded::Loading) => vec!["loading…".to_string()],
                Some(Loaded::Text(text)) => {
                    let lines: Vec<String> = clean(text)
                        .lines()
                        .take(POPUP_LINES)
                        .map(str::to_string)
                        .collect();
                    if lines.is_empty() {
                        vec!["no output".to_string()]
                    } else {
                        lines
                    }
                }
                Some(Loaded::Failed(error)) => vec![error.clone()],
            });
        }
        Kind::Color(_) | Kind::Inert => return None,
    };
    lines.extend(title.map(str::to_string));
    Some(lines)
}

/// `text` without terminal escape sequences or control characters other
/// than newlines. A tab becomes four spaces.
pub fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.next() {
                Some('[') => {
                    for ch in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

/// Run `argv` in `cwd` and return what it printed. A command that fails, or
/// runs past `limit`, gives an error to show in its place.
pub async fn run(argv: &[String], cwd: &Path, limit: Duration) -> Result<String, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("no command".to_string());
    };
    let child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(limit, child).await {
        Err(_) => {
            return Err(format!(
                "{program} timed out after {} s",
                limit.as_secs_f32()
            ));
        }
        Ok(Err(error)) => return Err(format!("{program}: {error}")),
        Ok(Ok(output)) => output,
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = clean(&String::from_utf8_lossy(&output.stderr));
    let reason = stderr.lines().next().unwrap_or_default().trim();
    Err(if reason.is_empty() {
        format!("{program}: {}", output.status)
    } else {
        reason.to_string()
    })
}

/// Start `argv` in `cwd` and let it go, output ignored.
pub fn detach(argv: &[String], cwd: &Path) {
    let Some((program, args)) = argv.split_first() else {
        return;
    };
    let child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn config() -> LinksConfig {
        toml::from_str(
            r#"
            [schemes.foam]
            hover = ["foam", "show", "{target}", "--plain"]
            [schemes.note]
            hover = ["cat", "{target}"]
            click = ["open-note", "{target}"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn a_target_is_read_by_its_scheme() {
        assert_eq!(classify("color:red"), Kind::Color("red"));
        assert_eq!(classify("COLOR:red"), Kind::Color("red"));
        assert_eq!(classify("https://x.test/a"), Kind::Url);
        assert_eq!(classify("http://x.test"), Kind::Url);
        assert_eq!(
            classify("foam:coax-12e8af"),
            Kind::Scheme {
                scheme: "foam",
                rest: "coax-12e8af"
            }
        );
        assert_eq!(classify("notes.md"), Kind::Inert);
        assert_eq!(classify("1x:y"), Kind::Inert);
        assert_eq!(classify(":y"), Kind::Inert);
    }

    #[test]
    fn a_file_target_has_a_path_and_an_optional_line() {
        assert_eq!(
            classify("file:src/ui.rs#40"),
            Kind::File {
                path: "src/ui.rs",
                line: Some(40)
            }
        );
        assert_eq!(
            classify("file:src/ui.rs"),
            Kind::File {
                path: "src/ui.rs",
                line: None
            }
        );
        assert_eq!(
            classify("file:///etc/hosts#L3"),
            Kind::File {
                path: "/etc/hosts",
                line: Some(3)
            }
        );
        assert_eq!(
            classify("file:a.rs#top"),
            Kind::File {
                path: "a.rs",
                line: None
            }
        );
        assert_eq!(classify("file:"), Kind::Inert);
    }

    #[test]
    fn the_hover_command_has_the_part_after_the_scheme_in_place_of_target() {
        assert_eq!(
            hover_command("foam:coax-1", &config()).unwrap(),
            ["foam", "show", "coax-1", "--plain"]
        );
        assert_eq!(hover_command("other:x", &config()), None);
        assert_eq!(hover_command("file:x", &config()), None);
    }

    #[test]
    fn a_target_that_would_pass_a_flag_runs_nothing() {
        assert_eq!(hover_command("foam:--help", &config()), None);
        assert_eq!(hover_command("foam:", &config()), None);
    }

    #[test]
    fn a_file_click_shows_the_absolute_path_at_the_line_in_the_configured_pane() {
        let cwd = PathBuf::from("/work/coax");
        let Some(Click::Show { to, show }) = click("file:src/ui.rs#40", &config(), &cwd) else {
            panic!("no show");
        };
        assert_eq!(to, "code");
        assert_eq!(show.kind, "file");
        assert_eq!(show.target, "/work/coax/src/ui.rs");
        assert_eq!(show.at, Some(json!(40)));
        let Some(Click::Show { show, .. }) = click("file:/abs/a.rs", &config(), &cwd) else {
            panic!("no show");
        };
        assert_eq!(show.target, "/abs/a.rs");
        assert_eq!(show.at, None);
    }

    #[test]
    fn a_url_click_runs_xdg_open_only_when_the_config_allows_it() {
        let cwd = PathBuf::new();
        assert_eq!(click("https://x.test", &config(), &cwd), None);
        let mut open = config();
        open.open_urls = true;
        assert_eq!(
            click("https://x.test", &open, &cwd),
            Some(Click::Run(vec![
                "xdg-open".to_string(),
                "https://x.test".to_string()
            ]))
        );
    }

    #[test]
    fn a_scheme_clicks_only_when_the_config_gives_it_a_click_command() {
        let cwd = PathBuf::new();
        assert_eq!(click("foam:x", &config(), &cwd), None);
        assert_eq!(
            click("note:a", &config(), &cwd),
            Some(Click::Run(vec!["open-note".to_string(), "a".to_string()]))
        );
    }

    #[test]
    fn the_popup_for_a_file_shows_the_relative_path_the_line_and_the_title() {
        let cwd = PathBuf::from("/work/coax");
        let cache = HashMap::new();
        assert_eq!(
            popup(
                "file:src/ui.rs#40",
                Some("the draw"),
                &config(),
                &cwd,
                &cache
            )
            .unwrap(),
            ["src/ui.rs:40", "the draw"]
        );
        assert_eq!(
            popup("file:/elsewhere/a.rs", None, &config(), &cwd, &cache).unwrap(),
            ["/elsewhere/a.rs"]
        );
    }

    #[test]
    fn the_popup_for_a_scheme_follows_the_cache() {
        let cwd = PathBuf::new();
        let mut cache = HashMap::new();
        let lines = |cache: &HashMap<String, Loaded>| popup("foam:a", None, &config(), &cwd, cache);
        assert_eq!(lines(&cache).unwrap(), ["loading…"]);
        cache.insert("foam:a".to_string(), Loaded::Text("one\ntwo\n".to_string()));
        assert_eq!(lines(&cache).unwrap(), ["one", "two"]);
        cache.insert("foam:a".to_string(), Loaded::Failed("boom".to_string()));
        assert_eq!(lines(&cache).unwrap(), ["boom"]);
        assert_eq!(popup("other:a", None, &config(), &cwd, &cache), None);
        assert_eq!(popup("color:red", None, &config(), &cwd, &cache), None);
    }

    #[test]
    fn the_popup_shows_the_first_fifteen_lines_of_output() {
        let text: String = (0..40).map(|n| format!("line {n}\n")).collect();
        let mut cache = HashMap::new();
        cache.insert("foam:a".to_string(), Loaded::Text(text));
        let lines = popup("foam:a", None, &config(), Path::new("."), &cache).unwrap();
        assert_eq!(lines.len(), POPUP_LINES);
        assert_eq!(lines[14], "line 14");
    }

    #[test]
    fn output_loses_its_escape_sequences() {
        assert_eq!(
            clean("\x1b[1;31mred\x1b[0m \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\\tend\r\n"),
            "red link    end\n"
        );
    }

    #[tokio::test]
    async fn a_command_gives_its_output() {
        let argv = ["echo".to_string(), "hi".to_string()];
        let out = run(&argv, Path::new("."), Duration::from_secs(5)).await;
        assert_eq!(out.unwrap(), "hi\n");
    }

    #[tokio::test]
    async fn a_command_that_runs_too_long_gives_a_timeout_error() {
        let argv: Vec<String> = ["sh", "-c", "sleep 5"].map(String::from).to_vec();
        let began = std::time::Instant::now();
        let out = run(&argv, Path::new("."), Duration::from_millis(100)).await;
        assert!(out.unwrap_err().contains("timed out"));
        assert!(began.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn a_command_that_fails_gives_its_first_error_line() {
        let argv: Vec<String> = ["sh", "-c", "echo no such issue >&2; exit 1"]
            .map(String::from)
            .to_vec();
        let out = run(&argv, Path::new("."), Duration::from_secs(5)).await;
        assert_eq!(out.unwrap_err(), "no such issue");
        let missing = ["coax-no-such-command".to_string()];
        let out = run(&missing, Path::new("."), Duration::from_secs(5)).await;
        assert!(out.unwrap_err().starts_with("coax-no-such-command"));
    }

    #[test]
    fn a_links_table_reads_and_the_defaults_hold_without_one() {
        let config: LinksConfig = toml::from_str(
            "decade_pane = \"files\"\nopen_urls = true\n[schemes.foam]\nhover = [\"foam\"]\n",
        )
        .unwrap();
        assert_eq!(config.decade_pane, "files");
        assert!(config.open_urls);
        assert_eq!(config.schemes["foam"].click, None);
        let default: LinksConfig = toml::from_str("").unwrap();
        assert_eq!(default, LinksConfig::default());
        assert_eq!(default.decade_pane, "code");
        assert!(!default.open_urls);
        assert!(toml::from_str::<LinksConfig>("pane = \"x\"").is_err());
    }
}
