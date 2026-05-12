use serde::Serialize;

/// Bump when the `_meta` shape changes in a way transcript readers must notice.
pub const SCHEMA_VERSION: u32 = 1;

/// `_meta` annotation that every relaywash tool result carries. Burn's annotation reader
/// (AgentWorkforce/burn#219) reads this to attribute savings; transcript-based learners
/// read it from the text content block. `response_bytes` is filled in by the MCP formatter
/// so individual tool authors do not have to re-derive payload size.
#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub replaces: Vec<String>,
    #[serde(rename = "collapsedCalls")]
    pub collapsed_calls: u32,
    #[serde(rename = "responseBytes", skip_serializing_if = "Option::is_none")]
    pub response_bytes: Option<u64>,
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
            schema_version: SCHEMA_VERSION,
        }
    }
}
