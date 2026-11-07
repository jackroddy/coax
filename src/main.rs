use agent_client_protocol::AcpAgent;
use coax::{acp, app};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut first_prompt = None;
    if args.first().map(String::as_str) == Some("--prompt") && args.len() > 1 {
        first_prompt = Some(args[1].clone());
        args.drain(..2);
    }
    if args.is_empty() {
        eprintln!("usage: coax [--prompt <text>] <agent command> [args...]");
        std::process::exit(2);
    }
    let agent = match AcpAgent::from_args(args) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("coax: {error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = acp::run(agent, async |link| app::run(link, first_prompt).await).await {
        ratatui::restore();
        eprintln!("coax: {error}");
        std::process::exit(1);
    }
}
