use serde::Serialize;
use serde_json::Value;

/// Bump when the `_meta` shape changes in a way transcript readers must notice.
pub const SCHEMA_VERSION: u32 = 1;

/// The key under which [`Meta`] is attached to a tool result's structured object.
pub const META_KEY: &str = "_meta";

/// `_meta` annotation that every relaywash tool result carries. Burn's annotation reader
/// (AgentWorkforce/burn#219) reads this to attribute savings; transcript-based learners
/// read it from the text content block. `response_bytes` is filled in by the MCP formatter
/// so individual tool authors do not have to re-derive payload size.
///
/// `baseline_bytes` is an optional, tool-supplied estimate of the vanilla output size.
/// Read prices it as full file bytes; the subprocess tools (Build/TestRun/GitState/GhPR)
/// price it via `crate::process::subprocess_baseline` (raw stdout+stderr bytes), which is
/// the single definition for those tools. The post-tool observe hook reads it to emit a
/// `tool_metrics` event with a savings delta.
#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub replaces: Vec<String>,
    #[serde(rename = "collapsedCalls")]
    pub collapsed_calls: u32,
    #[serde(rename = "responseBytes", skip_serializing_if = "Option::is_none")]
    pub response_bytes: Option<u64>,
    #[serde(rename = "baselineBytes", skip_serializing_if = "Option::is_none")]
    pub baseline_bytes: Option<u64>,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
}

impl Meta {
    pub fn new<I, S>(replaces: I, collapsed_calls: u32) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            replaces: replaces.into_iter().map(Into::into).collect(),
            collapsed_calls,
            response_bytes: None,
            baseline_bytes: None,
            schema_version: SCHEMA_VERSION,
        }
    }

    pub fn with_baseline(mut self, baseline_bytes: u64) -> Self {
        self.baseline_bytes = Some(baseline_bytes);
        self
    }

    /// Read `responseBytes` from the `_meta` attached to `container` (the structured
    /// object that directly holds `_meta`; the nesting differs by caller, so each
    /// passes its own container). `None` if absent or non-numeric.
    ///
    /// These readers live here, next to the serde renames above, so the `_meta` wire
    /// names have a single home — a reader can no longer silently drift from what the
    /// formatter writes when [`SCHEMA_VERSION`] is bumped.
    pub fn response_bytes_of(container: &Value) -> Option<u64> {
        meta_field_u64(container, "responseBytes")
    }

    /// Read `baselineBytes` from the `_meta` attached to `container`. See
    /// [`response_bytes_of`](Self::response_bytes_of).
    pub fn baseline_bytes_of(container: &Value) -> Option<u64> {
        meta_field_u64(container, "baselineBytes")
    }
}

fn meta_field_u64(container: &Value, field: &str) -> Option<u64> {
    container.get(META_KEY)?.get(field)?.as_u64()
}
