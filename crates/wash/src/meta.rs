use serde::Serialize;

/// `_meta` annotation that every relaywash tool result carries. Burn's annotation reader
/// (AgentWorkforce/burn#219) reads this to attribute savings.
#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub replaces: Vec<String>,
    #[serde(rename = "collapsedCalls")]
    pub collapsed_calls: u32,
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
        }
    }
}
