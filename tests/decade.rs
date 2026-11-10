use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use agent_client_protocol::Error;
use agent_client_protocol::v1::schema::{
    PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
};
use coax::acp::{self, Event, Link};
use coax::fake;
use coax::report::{self, Reporter, Snapshot};
use coax::scenario::Scenario;
use coax::session::Transcript;
use coax::settings::Settings;

const HELLO: &str = include_str!("../scenarios/hello.json");

fn socket_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("coax-decade-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn report(reporter: &mut Reporter, transcript: &Transcript, question: Option<&str>) {
    let now = Snapshot::of(transcript, question, &Settings::default());
    reporter.send(now).unwrap();
}

async fn one_turn(mut link: Link, mut reporter: Reporter) -> Result<(), Error> {
    link.initialize().await?;
    let session = link.new_session(std::env::temp_dir()).await?.session_id;
    let mut transcript = Transcript::default();
    report(&mut reporter, &transcript, None);
    transcript.prompt("hello".to_string());
    link.prompt(session, "hello".to_string())?;
    report(&mut reporter, &transcript, None);
    while let Some(event) = link.events.recv().await {
        match event {
            Event::Update(update) => transcript.apply(*update),
            Event::Permission {
                update,
                options,
                reply,
            } => {
                report(&mut reporter, &transcript, update.fields.title.as_deref());
                let allow = options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                    .unwrap();
                reply
                    .send(RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(allow.option_id.clone()),
                    ))
                    .unwrap();
                transcript.permission(*update, allow.name.clone());
            }
            Event::Tokens(_) | Event::Setting { .. } | Event::Exited(_) | Event::Form { .. } => {}
            Event::Done(result) => {
                transcript.finished(result?);
                report(&mut reporter, &transcript, None);
                break;
            }
        }
        report(&mut reporter, &transcript, None);
    }
    reporter.end();
    Ok(())
}

#[tokio::test]
async fn a_turn_with_a_tool_call_and_a_permission_reaches_decade_as_these_lines() {
    let path = socket_dir("turn").join("decade.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let lines = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut lines = Vec::new();
        for line in BufReader::new(stream).lines() {
            let line = line.unwrap();
            let ended = line == r#"{"ended":{}}"#;
            lines.push(line);
            if ended {
                break;
            }
        }
        lines
    });
    let identity = decade_channel::Agent {
        name: "fake".to_string(),
        version: "1.2.3".to_string(),
        session: "fake-session".to_string(),
    };
    let (reporter, _messages) = report::connect(&path, Some("chat"), &identity).unwrap();
    let scenario = Scenario::parse(HELLO).unwrap();
    acp::run(fake::agent(scenario), "fake", async |link| {
        one_turn(link, reporter).await
    })
    .await
    .unwrap();
    let lines = lines.join().unwrap();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    assert_eq!(
        lines,
        [
            r#"{"hello":{"name":"coax","accepts":[],"pane":"chat","shows":[],"agent":{"name":"fake","version":"1.2.3","session":"fake-session"}}}"#,
            r#"{"agent":{"state":"idle"}}"#,
            r#"{"agent":{"state":"working"}}"#,
            r#"{"agent":{"activity":"Read src/main.rs"}}"#,
            r#"{"agent":{"activity":null}}"#,
            r#"{"agent":{"state":"attention","question":"Write NOTES.md"}}"#,
            r#"{"agent":{"state":"working","activity":"Write NOTES.md","question":null}}"#,
            r#"{"agent":{"activity":null}}"#,
            r#"{"agent":{"state":"done"}}"#,
            r#"{"ended":{}}"#,
        ]
    );
}

#[test]
fn a_show_reaches_the_named_pane_with_its_kind_target_and_line() {
    let path = socket_dir("show").join("show.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let lines = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        BufReader::new(stream)
            .lines()
            .map(|line| line.unwrap())
            .take(2)
            .collect::<Vec<_>>()
    });
    let identity = decade_channel::Agent {
        name: "fake".to_string(),
        version: "1".to_string(),
        session: "s".to_string(),
    };
    let (mut reporter, _messages) = report::connect(&path, Some("chat"), &identity).unwrap();
    let show = decade_channel::Show {
        kind: "file".to_string(),
        target: "/work/src/ui.rs".to_string(),
        at: Some(serde_json::json!(40)),
    };
    reporter.show("code", &show).unwrap();
    let lines = lines.join().unwrap();
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    let sent: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(
        sent,
        serde_json::json!({"to": "code", "show": {"kind": "file", "target": "/work/src/ui.rs", "at": 40}})
    );
}
