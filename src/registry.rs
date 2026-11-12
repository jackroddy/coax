use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

const RAW_URL: &str = "https://raw.githubusercontent.com/agentclientprotocol/registry/main";

/// How long a fetched version is trusted.
const FRESH_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a fetch may take.
const FETCH_LIMIT: Duration = Duration::from_secs(5);

/// The registry's version of an agent, as kept in the cache file.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Cached {
    version: String,

    /// Seconds since the epoch when the version was fetched.
    fetched: u64,
}

/// The stable part of a version as numbers: `0.101.1-preview.7` is
/// `[0, 101, 1]`. A version that does not read this way is `None`.
fn stable(version: &str) -> Option<Vec<u64>> {
    let core = version.split(['-', '+']).next()?;
    core.split('.').map(|part| part.parse().ok()).collect()
}

/// Whether the registry's `latest` is a later release than `installed`.
pub fn is_newer(latest: &str, installed: &str) -> bool {
    let (Some(mut latest), Some(mut installed)) = (stable(latest), stable(installed)) else {
        return false;
    };
    let len = latest.len().max(installed.len());
    latest.resize(len, 0);
    installed.resize(len, 0);
    latest > installed
}

fn is_fresh(fetched: u64, now: u64) -> bool {
    now.saturating_sub(fetched) < FRESH_FOR.as_secs()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "-_.".contains(ch))
        && !id.starts_with('.')
}

/// The cache file of agent `id`, under `$XDG_CACHE_HOME/coax` or `~/.cache/coax`.
fn cache_path(id: &str) -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    Some(base.join("coax").join(format!("registry-{id}.json")))
}

fn read_cache(path: &PathBuf, now: u64) -> Option<String> {
    let cached: Cached = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    is_fresh(cached.fetched, now).then_some(cached.version)
}

async fn fetch(id: &str) -> Option<String> {
    let url = format!("{RAW_URL}/{id}/agent.json");
    let curl = Command::new("curl")
        .args(["--fail", "--silent", "--location", "--proto", "=https"])
        .arg("--max-time")
        .arg(FETCH_LIMIT.as_secs().to_string())
        .arg(url)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(FETCH_LIMIT + Duration::from_secs(1), curl)
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    #[derive(Deserialize)]
    struct Entry {
        version: String,
    }
    serde_json::from_slice::<Entry>(&output.stdout)
        .ok()
        .map(|entry| entry.version)
}

/// The registry's latest version of agent `id`, from the cache when it is
/// under a day old and from the network otherwise. A failure gives `None`.
pub async fn latest(id: &str) -> Option<String> {
    if !valid_id(id) {
        return None;
    }
    let path = cache_path(id);
    if let Some(version) = path.as_ref().and_then(|path| read_cache(path, now())) {
        return Some(version);
    }
    let version = fetch(id).await?;
    if let Some(path) = path {
        let cached = Cached {
            version: version.clone(),
            fetched: now(),
        };
        let _ = std::fs::create_dir_all(path.parent()?);
        let _ = std::fs::write(path, serde_json::to_string(&cached).ok()?);
    }
    Some(version)
}

/// The registry's version of agent `id` when it is later than `installed`.
pub async fn newer_than(id: &str, installed: &str) -> Option<String> {
    latest(id)
        .await
        .filter(|latest| is_newer(latest, installed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_release_is_newer_by_number_and_not_by_text() {
        assert!(is_newer("0.101.0", "0.99.9"));
        assert!(is_newer("1.0.0", "0.101.0"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("0.101.0", "0.101.0"));
        assert!(!is_newer("0.100.0", "0.101.0"));
        assert!(is_newer("1.2.1", "1.2"));
        assert!(!is_newer("1.2", "1.2.0"));
    }

    #[test]
    fn a_preview_suffix_is_ignored_on_both_sides() {
        assert!(!is_newer("0.101.1", "0.101.1-preview.7"));
        assert!(is_newer("0.101.2", "0.101.1-preview.7"));
        assert!(!is_newer("0.101.1-preview.9", "0.101.1"));
        assert!(!is_newer("0.101.0+build5", "0.101.0"));
    }

    #[test]
    fn a_version_that_is_not_numbers_is_never_newer() {
        assert!(!is_newer("latest", "0.1.0"));
        assert!(!is_newer("0.2.0", ""));
        assert!(!is_newer("0.2.x", "0.1.0"));
    }

    #[test]
    fn a_cached_version_is_fresh_for_a_day() {
        assert!(is_fresh(1000, 1000));
        assert!(is_fresh(1000, 1000 + 24 * 60 * 60 - 1));
        assert!(!is_fresh(1000, 1000 + 24 * 60 * 60));
        assert!(is_fresh(2000, 1000));
    }

    #[test]
    fn the_cache_file_gives_a_fresh_version_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("coax-registry-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("registry-x.json");
        let write = |fetched: u64| {
            let cached = Cached {
                version: "0.3.0".to_string(),
                fetched,
            };
            std::fs::write(&path, serde_json::to_string(&cached).unwrap()).unwrap();
        };
        write(5000);
        assert_eq!(read_cache(&path, 5100).as_deref(), Some("0.3.0"));
        assert_eq!(read_cache(&path, 5000 + 90_000), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_cache(&path, 5100), None);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read_cache(&path, 5100), None);
    }

    #[test]
    fn an_id_that_could_leave_the_registry_path_is_refused() {
        assert!(valid_id("claude-acp"));
        assert!(!valid_id(""));
        assert!(!valid_id("../x"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id(".hidden"));
    }
}
