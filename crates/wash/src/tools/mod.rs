pub mod build;
pub mod edit;
pub mod gh_pr;
pub mod git_state;
pub mod read;
pub mod search;
pub mod test_run;

use anyhow::Result;
use serde_json::Value;

use crate::mcp::{Tool, ToolResult};
use crate::meta::Meta;

/// Wrap a tool's JSON payload in a `ToolResult` carrying the standard `_meta`
/// annotation. Every process-backed tool funnels through here so the `replaces`
/// label, `collapsedCalls`, and optional `baselineBytes` are constructed one way
/// — a future `Meta` change can't silently miss a tool.
pub(crate) fn ok_with_meta(
    tool_name: &str,
    replaces: &str,
    value: Value,
    baseline: Option<u64>,
) -> Result<ToolResult> {
    let mut meta = Meta::new([replaces.to_string()], 1);
    if let Some(bytes) = baseline {
        meta = meta.with_baseline(bytes);
    }
    Ok(ToolResult::new(tool_name, value).with_meta(meta))
}

pub fn all() -> Vec<Tool> {
    vec![
        search::tool(),
        read::tool(),
        edit::tool(),
        git_state::tool(),
        test_run::tool(),
        build::tool(),
        gh_pr::tool(),
    ]
}
