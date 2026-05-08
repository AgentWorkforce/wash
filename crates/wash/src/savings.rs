//! `wash savings` subcommand — pretty-print the relayburn ledger summary for a session
//! (or the global aggregate). Mirrors the legacy JS slash command.

use anyhow::Result;

use crate::burn::Ledger;

pub fn run(session: Option<&str>) -> Result<()> {
    let ledger = Ledger::default();
    let summary = ledger.summary(session);
    let label = session.unwrap_or("(all sessions)");
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("relaywash savings — session {label}"));
    lines.push(String::new());
    lines.push(format!("total tool calls: {}", summary.total_calls));
    lines.push(format!(
        "collapsed (built-in equivalents avoided): {}",
        summary.collapsed_calls
    ));
    let replaced = if summary.replaced_tools.is_empty() {
        "(none)".to_string()
    } else {
        summary.replaced_tools.join(", ")
    };
    lines.push(format!("replaced built-ins: {replaced}"));
    lines.push(String::new());
    lines.push("by tool:".into());
    for (name, info) in &summary.by_tool {
        lines.push(format!(
            "  {:<28} calls={}  collapsed={}",
            name, info.calls, info.collapsed_calls
        ));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}
