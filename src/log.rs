use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_client_protocol::{AcpAgent, LineDirection};
use serde_json::{Map, Value, json};

/// How many log files a start leaves in the directory, the new one included.
const KEEP: usize = 20;

/// How much a run writes. Each level includes the ones before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off,
    Info,
    Debug,
}

impl Level {
    /// The level `COAX_LOG` names. A missing or unknown value is `info`.
    pub fn parse(value: Option<&str>) -> Level {
        match value {
            Some("off") => Level::Off,
            Some("debug") => Level::Debug,
            _ => Level::Info,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
}

struct Logger {
    level: Level,
    path: PathBuf,
    file: Mutex<File>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Why the log file could not be made, for one notice on screen.
static PROBLEM: OnceLock<String> = OnceLock::new();

/// Open this run's log file at the level `COAX_LOG` names, and route panics
/// into it. When the file cannot be made, the run goes on without one.
pub fn init() {
    let level = Level::parse(std::env::var("COAX_LOG").ok().as_deref());
    if level == Level::Off {
        return;
    }
    let Some(dir) = dir(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME")) else {
        let _ = PROBLEM.set("no log: neither XDG_STATE_HOME nor HOME is set".to_string());
        return;
    };
    open(&dir, level, now_secs(), std::process::id());
}

pub(crate) fn open(dir: &Path, level: Level, secs: u64, pid: u32) {
    match Logger::create(dir, level, secs, pid) {
        Ok(logger) => {
            if LOGGER.set(logger).is_ok() {
                install_panic_hook();
            }
        }
        Err(error) => {
            let _ = PROBLEM.set(format!("no log in {}: {error}", dir.display()));
        }
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        info(
            "panic",
            json!({
                "message": panic.to_string(),
                "backtrace": std::backtrace::Backtrace::force_capture().to_string(),
            }),
        );
        previous(panic);
    }));
}

/// The directory the logs go in: `coax/logs` under `$XDG_STATE_HOME` or
/// `~/.local/state`.
fn dir(state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match state_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(home?).join(".local").join("state"),
    };
    Some(base.join("coax").join("logs"))
}

/// The path of this run's log file, if there is one.
pub fn path() -> Option<&'static Path> {
    LOGGER.get().map(|logger| logger.path.as_path())
}

/// What went wrong making the log file, if it did.
pub fn problem() -> Option<&'static str> {
    PROBLEM.get().map(String::as_str)
}

pub fn enabled(level: Level) -> bool {
    LOGGER.get().is_some_and(|logger| level <= logger.level)
}

pub fn info(event: &str, fields: Value) {
    if let Some(logger) = LOGGER.get() {
        logger.record(Level::Info, event, fields);
    }
}

pub fn debug(event: &str, fields: Value) {
    if let Some(logger) = LOGGER.get() {
        logger.record(Level::Debug, event, fields);
    }
}

/// Log what `agent` writes: its stderr lines at `info`, and every JSON-RPC
/// line in both directions at `debug`, as the transport reads and writes
/// them. `name` tells the agents of a picker apart.
pub fn tap(agent: AcpAgent, name: &str) -> AcpAgent {
    if !enabled(Level::Info) {
        return agent;
    }
    let name = name.to_string();
    agent.with_debug(move |line, direction| {
        let direction = match direction {
            LineDirection::Stderr => {
                info("agent.stderr", json!({"agent": name, "line": line}));
                return;
            }
            LineDirection::Stdin => "to_agent",
            LineDirection::Stdout => "from_agent",
        };
        if enabled(Level::Debug) {
            debug(
                "acp",
                json!({"agent": name, "direction": direction, "line": line}),
            );
        }
    })
}

impl Logger {
    /// Make the directory and a new file in it, and delete the oldest files
    /// past `KEEP`.
    fn create(dir: &Path, level: Level, secs: u64, pid: u32) -> io::Result<Logger> {
        fs::create_dir_all(dir)?;
        let path = dir.join(file_name(secs, pid));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let _ = prune(dir, KEEP);
        Ok(Logger {
            level,
            path,
            file: Mutex::new(file),
        })
    }

    /// Write one line: `t`, `level` and `event`, then the members of
    /// `fields`. A line above this logger's level is dropped.
    fn record(&self, level: Level, event: &str, fields: Value) {
        if level > self.level {
            return;
        }
        let mut line = Map::new();
        line.insert("t".to_string(), Value::String(timestamp()));
        line.insert("level".to_string(), Value::String(level.name().to_string()));
        line.insert("event".to_string(), Value::String(event.to_string()));
        if let Value::Object(fields) = fields {
            line.extend(fields);
        }
        let mut text = Value::Object(line).to_string();
        text.push('\n');
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(text.as_bytes());
        }
    }
}

/// `20261109T120000Z-4242.jsonl`: the start time, then the process id. File
/// names sort in start order.
fn file_name(secs: u64, pid: u32) -> String {
    let (date, time) = civil(secs);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z-{pid}.jsonl",
        date.0, date.1, date.2, time.0, time.1, time.2
    )
}

/// Delete every log file in `dir` but the newest `keep`.
fn prune(dir: &Path, keep: usize) -> io::Result<()> {
    let mut logs: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    logs.sort();
    for old in &logs[..logs.len().saturating_sub(keep)] {
        fs::remove_file(old)?;
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn timestamp() -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let (date, time) = civil(since.as_secs());
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        date.0,
        date.1,
        date.2,
        time.0,
        time.1,
        time.2,
        since.subsec_millis()
    )
}

/// UTC `((year, month, day), (hour, minute, second))` of a Unix time.
fn civil(secs: u64) -> ((u64, u64, u64), (u64, u64, u64)) {
    let days = secs / 86_400;
    let rest = secs % 86_400;
    // see: https://howardhinnant.github.io/date_algorithms.html#civil_from_days
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (
        (year, month, day),
        (rest / 3_600, rest % 3_600 / 60, rest % 60),
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn scratch() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "coax-log-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn lines(logger: &Logger) -> Vec<Value> {
        fs::read_to_string(&logger.path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_file_is_named_by_its_start_time_and_the_process_id() {
        assert_eq!(file_name(0, 7), "19700101T000000Z-7.jsonl");
        assert_eq!(
            file_name(1_794_225_600, 4242),
            "20261109T120000Z-4242.jsonl"
        );
        assert_eq!(file_name(951_782_400, 1), "20000229T000000Z-1.jsonl");
    }

    #[test]
    fn the_directory_is_under_xdg_state_home_or_the_home_directory() {
        assert_eq!(
            dir(Some("/state".into()), Some("/home/a".into())),
            Some(PathBuf::from("/state/coax/logs"))
        );
        assert_eq!(
            dir(Some("".into()), Some("/home/a".into())),
            Some(PathBuf::from("/home/a/.local/state/coax/logs"))
        );
        assert_eq!(dir(None, None), None);
    }

    #[test]
    fn a_new_file_is_private_and_makes_its_missing_directory() {
        let dir = scratch().join("a").join("b");
        let logger = Logger::create(&dir, Level::Info, 100, 1).unwrap();
        let mode = fs::metadata(&logger.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::remove_dir_all(dir.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_start_keeps_the_newest_twenty_files() {
        let dir = scratch();
        for secs in 0..25 {
            Logger::create(&dir, Level::Info, secs, 1).unwrap();
        }
        fs::write(dir.join("notes.txt"), "not a log").unwrap();
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".jsonl"))
            .collect();
        names.sort();
        assert_eq!(names.len(), KEEP);
        assert_eq!(names[0], file_name(5, 1));
        assert_eq!(names[KEEP - 1], file_name(24, 1));
        assert!(dir.join("notes.txt").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_line_is_one_json_object_with_the_time_level_event_and_fields() {
        let dir = scratch();
        let logger = Logger::create(&dir, Level::Debug, 100, 1).unwrap();
        logger.record(
            Level::Info,
            "notice",
            json!({"text": "hello \"there\"\nnow"}),
        );
        let text = fs::read_to_string(&logger.path).unwrap();
        assert_eq!(text.lines().count(), 1);
        let line = &lines(&logger)[0];
        let t = line["t"].as_str().unwrap();
        assert_eq!(t.len(), "2026-11-09T12:00:00.000Z".len());
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T');
        assert_eq!(line["level"], "info");
        assert_eq!(line["event"], "notice");
        assert_eq!(line["text"], "hello \"there\"\nnow");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn info_drops_debug_lines_and_debug_keeps_both() {
        let dir = scratch();
        let info = Logger::create(&dir, Level::Info, 1, 1).unwrap();
        info.record(Level::Info, "a", json!({}));
        info.record(Level::Debug, "b", json!({}));
        let events: Vec<_> = lines(&info).iter().map(|l| l["event"].clone()).collect();
        assert_eq!(events, ["a"]);
        let debug = Logger::create(&dir, Level::Debug, 2, 1).unwrap();
        debug.record(Level::Info, "a", json!({}));
        debug.record(Level::Debug, "b", json!({}));
        assert_eq!(lines(&debug).len(), 2);
        let off = Logger::create(&dir, Level::Off, 3, 1).unwrap();
        off.record(Level::Info, "a", json!({}));
        assert!(lines(&off).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn coax_log_reads_off_info_and_debug_and_defaults_to_info() {
        assert_eq!(Level::parse(Some("off")), Level::Off);
        assert_eq!(Level::parse(Some("debug")), Level::Debug);
        assert_eq!(Level::parse(Some("info")), Level::Info);
        assert_eq!(Level::parse(Some("loud")), Level::Info);
        assert_eq!(Level::parse(None), Level::Info);
    }

    #[test]
    fn a_directory_that_cannot_be_made_is_an_error_and_not_a_panic() {
        let dir = scratch();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        fs::write(&file, "").unwrap();
        assert!(Logger::create(&file.join("logs"), Level::Info, 1, 1).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
