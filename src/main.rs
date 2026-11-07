use agent_client_protocol::AcpAgent;
use coax::{acp, app};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut first_prompt = None;
    let mut auto_allow = false;
    loop {
        match args.first().map(String::as_str) {
            Some("--prompt") if args.len() > 1 => {
                first_prompt = Some(args[1].clone());
                args.drain(..2);
            }
            Some("--allow") => {
                auto_allow = true;
                args.remove(0);
            }
            _ => break,
        }
    }
    if args.is_empty() {
        eprintln!("usage: coax [--prompt <text>] [--allow] <agent command> [args...]");
        std::process::exit(2);
    }
    let agent = match AcpAgent::from_args(args) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("coax: {error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = acp::run(agent, async |link| {
        app::run(link, first_prompt, auto_allow).await
    })
    .await
    {
        ratatui::restore();
        eprintln!("coax: {error}");
        std::process::exit(1);
    }
}
