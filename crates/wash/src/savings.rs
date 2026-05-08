//! `wash savings` subcommand — pretty-print the relayburn ledger summary for a session
//! (or the global aggregate). Backed by `relayburn_sdk::summary`.

use anyhow::Result;
use relayburn_sdk::{SummaryOptions, summary};

pub fn run(session: Option<&str>) -> Result<()> {
    let opts = SummaryOptions {
        session: session.map(|s| s.to_string()),
        ..Default::default()
    };
    let s = summary(opts)?;
    let label = session.unwrap_or("(all sessions)");
    let savings = s.replacement_savings.unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("relaywash savings — session {label}"));
    lines.push(String::new());
    lines.push(format!("total replacement-tool calls: {}", savings.calls));
    lines.push(format!(
        "collapsed (built-in equivalents avoided): {}",
        savings.collapsed_calls
    ));
    lines.push(format!(
        "estimated tokens saved: {}",
        savings.estimated_tokens_saved
    ));
    lines.push(String::new());
    lines.push("by tool:".into());
    for (name, agg) in &savings.by_tool {
        lines.push(format!(
            "  {:<28} calls={}  collapsed={}  tokens_saved={}",
            name, agg.calls, agg.collapsed_calls, agg.estimated_tokens_saved
        ));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}
