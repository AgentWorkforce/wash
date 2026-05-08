use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wash", version, about = "relaywash — clean agent tool output, lower token burn")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP stdio server (default action).
    Mcp,
    /// Run a hook handler. `kind` matches the entry registered in hooks/hooks.json.
    Hook {
        kind: String,
    },
    /// Print the relayburn savings summary for a session (or all sessions).
    Savings {
        /// Session id. Omit to aggregate across every session in the ledger.
        #[arg(long)]
        session: Option<String>,
    },
    /// Compare replacement vs vanilla bytes against the fixture corpus. Reserved for later PR.
    BurnCompare,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Mcp) {
        Command::Mcp => wash::mcp::serve(),
        Command::Hook { kind } => wash::hooks::run(&kind),
        Command::Savings { session } => wash::savings::run(session.as_deref()),
        Command::BurnCompare => {
            eprintln!("wash burn-compare: not implemented yet (later PR)");
            std::process::exit(2);
        }
    }
}
