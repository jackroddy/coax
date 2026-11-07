use agent_client_protocol::{ConnectTo, Error, Stdio};
use coax::fake;
use coax::scenario::Scenario;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: coax-fake-agent <scenario.json>");
        std::process::exit(2);
    };
    let json = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        eprintln!("coax-fake-agent: cannot read {path}: {error}");
        std::process::exit(2);
    });
    let scenario = Scenario::parse(&json).unwrap_or_else(|error| {
        eprintln!("coax-fake-agent: {path} is not a scenario: {error}");
        std::process::exit(2);
    });
    fake::agent(scenario).connect_to(Stdio::new()).await
}
