//! Editing the prompt in the user's editor.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

/// The editor to run: `$VISUAL`, else `$EDITOR`, else `vi`.
pub fn pick(visual: Option<String>, editor: Option<String>) -> String {
    [visual, editor]
        .into_iter()
        .flatten()
        .find(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string())
}

/// Run `editor` on a temporary file holding `text`, with the terminal's
/// stdio inherited, and return the saved text without its trailing newline.
/// The error is a line for the conversation.
pub fn edit(editor: &str, text: &str) -> Result<String, String> {
    let path = temp_path();
    let outcome = run(editor, &path, text);
    let _ = fs::remove_file(&path);
    outcome
}

fn run(editor: &str, path: &PathBuf, text: &str) -> Result<String, String> {
    fs::write(path, text)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    // the shell splits an editor setting such as `code --wait`
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .map_err(|error| format!("could not run {editor}: {error}"))?;
    if !status.success() {
        return Err(format!("{editor} exited with {status}, prompt unchanged"));
    }
    let saved = fs::read(path)
        .map_err(|error: io::Error| format!("could not read the file back: {error}"))?;
    let saved = String::from_utf8_lossy(&saved);
    Ok(saved.trim_end_matches(['\n', '\r']).to_string())
}

fn temp_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    std::env::temp_dir().join(format!("coax-prompt-{}-{nanos}.md", std::process::id()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn script(name: &str, body: &str) -> String {
        let path = std::env::temp_dir().join(format!("coax-test-{}-{name}", std::process::id()));
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn visual_wins_over_editor_and_vi_is_the_last_resort() {
        let some = |name: &str| Some(name.to_string());
        assert_eq!(pick(some("code -w"), some("nano")), "code -w");
        assert_eq!(pick(None, some("nano")), "nano");
        assert_eq!(pick(some(" "), some("nano")), "nano");
        assert_eq!(pick(None, None), "vi");
    }

    #[test]
    fn the_saved_text_comes_back_without_its_trailing_newline() {
        let editor = script("rewrite", r#"printf 'new 日本\nsecond line\n\n' > "$1""#);
        let text = edit(&editor, "old").unwrap();
        assert_eq!(text, "new 日本\nsecond line");
        let _ = fs::remove_file(editor);
    }

    #[test]
    fn the_editor_gets_the_prompt_text_in_the_file() {
        let seen = std::env::temp_dir().join(format!("coax-test-{}-seen", std::process::id()));
        let editor = script("copy", &format!(r#"cat "$1" > {}"#, seen.display()));
        let text = edit(&editor, "draft\nover two lines").unwrap();
        assert_eq!(text, "draft\nover two lines");
        assert_eq!(fs::read_to_string(&seen).unwrap(), "draft\nover two lines");
        let _ = fs::remove_file(editor);
        let _ = fs::remove_file(seen);
    }

    #[test]
    fn an_editor_that_exits_non_zero_is_an_error() {
        let editor = script("fail", r#"printf x > "$1"; exit 3"#);
        let error = edit(&editor, "old").unwrap_err();
        assert!(error.contains("exited with"), "{error}");
        assert!(error.contains("prompt unchanged"), "{error}");
        let _ = fs::remove_file(editor);
    }

    #[test]
    fn an_editor_that_does_not_exist_is_an_error() {
        let error = edit("/no/such/editor", "old").unwrap_err();
        assert!(error.contains("prompt unchanged"), "{error}");
    }

    #[test]
    fn an_editor_setting_with_arguments_gets_the_file_last() {
        let editor = script("args", r#"printf '%s' "$1" > "$2""#);
        let text = edit(&format!("{editor} --wait"), "x").unwrap();
        assert_eq!(text, "--wait");
        let _ = fs::remove_file(editor);
    }
}
