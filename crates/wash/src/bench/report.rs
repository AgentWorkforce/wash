//! Serializable shapes for benchmark runs. Stored as JSON so the comparator can be
//! pointed at any two reports (baseline vs current, before-PR vs after-PR, etc.)
//! without re-running the harness.
//!
//! The shape is deliberately verbose: one record per tool call, plus per-task and
//! suite-level aggregates. That way the comparator can attribute byte/token deltas
//! to a specific call rather than just to a task in aggregate.

use serde::{Deserialize, Serialize};

/// Top-level report: one suite run. A suite is a set of tasks executed against
/// a set of corpora.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteReport {
    /// Schema version. Bump when fields are removed or renamed so the comparator
    /// can refuse to diff mismatched shapes.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Human label — what we're benchmarking. Free-form (e.g. "main@abcd123").
    pub label: String,
    /// Tasks executed in this run, in order.
    pub tasks: Vec<TaskReport>,
    /// Aggregate counts across all tasks. Handy for top-line regression checks.
    pub totals: RunReport,
}

/// Aggregate metrics for a task (or whole suite). Bytes/tokens stay separate so
/// the comparator can report both — token estimates depend on the model, bytes
/// don't.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    /// Number of tool calls executed.
    #[serde(rename = "callCount")]
    pub call_count: u32,
    /// Sum of `responseBytes` across all calls.
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    /// Sum of estimated tokens across all calls. Derived from `total_bytes` via
    /// `crate::tokens::estimate_tokens` so it's stable for a given byte count.
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    /// True if any call hit its result/output cap (e.g. Search truncated).
    #[serde(rename = "anyCapHit")]
    pub any_cap_hit: bool,
    /// True if the same tool was called with the exact same args more than once
    /// during the task. Useful as a "didn't drop into a retry loop" sanity check.
    #[serde(rename = "anyRepeatedCall")]
    pub any_repeated_call: bool,
}

/// One benchmark task: the calls made and how they scored against expectations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReport {
    /// Task identifier — matches the `fixtures/bench/<name>/` directory.
    pub name: String,
    /// Per-call breakdown, in execution order.
    pub calls: Vec<CallRecord>,
    /// Pass/fail/skipped against each expectation defined in `expectations.json`.
    pub expectations: Vec<ExpectationOutcome>,
    /// Aggregate metrics for this task.
    pub totals: RunReport,
    /// True when every expectation passed and there are no caps or repeats marked
    /// "fail on" in the expectations file.
    pub passed: bool,
}

/// One call record. Captures both what was requested and what came back so the
/// comparator can detect arg drift in addition to size drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecord {
    /// Step label from the expectations file (e.g. "search-symbol").
    pub step: String,
    /// MCP tool name (e.g. `relaywash__Search`).
    pub tool: String,
    /// Args passed to the tool — verbatim from the expectations file. Stored so
    /// repeated-call detection (`anyRepeatedCall`) can canonicalize correctly.
    pub args: serde_json::Value,
    /// `responseBytes` from the result's `_meta` block, falling back to the
    /// compact-JSON encoding length if the tool didn't attach a Meta.
    #[serde(rename = "responseBytes")]
    pub response_bytes: u64,
    /// Estimated tokens for `response_bytes`.
    #[serde(rename = "responseTokens")]
    pub response_tokens: u64,
    /// `baselineBytes` from `_meta`, when the tool reported a vanilla baseline.
    /// `None` for tools that don't emit one.
    #[serde(rename = "baselineBytes", skip_serializing_if = "Option::is_none")]
    pub baseline_bytes: Option<u64>,
    /// Whether the tool indicated truncation (Search `truncated`, Read `truncated`,
    /// etc.). Stored separately from `any_cap_hit` on the aggregate so a single
    /// expected-truncation case doesn't poison the suite.
    #[serde(rename = "capHit")]
    pub cap_hit: bool,
    /// True if the result came back as an error envelope (`isError: true`).
    pub error: bool,
}

/// Result of evaluating one expectation rule against the calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectationOutcome {
    pub name: String,
    pub passed: bool,
    /// Human description of what went wrong, when `passed == false`. Empty on
    /// pass.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl RunReport {
    /// Fold a CallRecord into running totals. Used both per-task and per-suite.
    pub fn fold_call(&mut self, call: &CallRecord) {
        self.call_count = self.call_count.saturating_add(1);
        self.total_bytes = self.total_bytes.saturating_add(call.response_bytes);
        self.total_tokens = self.total_tokens.saturating_add(call.response_tokens);
        if call.cap_hit {
            self.any_cap_hit = true;
        }
    }

    /// Merge another report into this one. Used when rolling task totals up to
    /// suite totals.
    pub fn merge(&mut self, other: &RunReport) {
        self.call_count = self.call_count.saturating_add(other.call_count);
        self.total_bytes = self.total_bytes.saturating_add(other.total_bytes);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.any_cap_hit = self.any_cap_hit || other.any_cap_hit;
        self.any_repeated_call = self.any_repeated_call || other.any_repeated_call;
    }
}
