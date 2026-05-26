//! Benchmark task runner. Reads `fixtures/bench/<task>/expectations.json`, drives
//! the same tool handlers the MCP server registers, and produces a `TaskReport`.
//!
//! Calling the real tool handlers (rather than parsing transcripts or re-implementing
//! the logic) is the whole point: any change in Search/Read output shows up here,
//! and any optimization to those tools shifts the harness numbers immediately.

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::bench::report::{
    CallRecord, ExpectationOutcome, RunReport, SuiteReport, TaskReport,
};
use crate::mcp::{Tool, ToolContext, format_tool_result};
use crate::meta::SCHEMA_VERSION;
use crate::tokens::estimate_tokens;
use crate::tools;

/// Schema bump rule: when we change `TaskReport` or `CallRecord` in a breaking way,
/// bump this. Independent from `meta::SCHEMA_VERSION` so the bench report schema can
/// move without dragging the tool-result meta schema with it.
pub const BENCH_SCHEMA_VERSION: u32 = 1;

/// Options for a suite run.
pub struct RunOptions {
    /// Repository root — used to locate `fixtures/bench/` and `fixtures/corpus/`.
    pub repo_root: PathBuf,
    /// Restrict to these task names. Empty = run them all.
    pub tasks: Vec<String>,
    /// "fast" — only tasks whose expectations file sets `"fastSubset": true`.
    /// CI hook lives here: pass `--fast` and the runner picks the cheap ones.
    pub fast_only: bool,
    /// Label written into the suite report. Free-form (e.g. "main@abcd123").
    pub label: String,
}

/// Expectations file shape. One per task, lives at
/// `fixtures/bench/<task>/expectations.json`.
#[derive(Debug, Clone, Deserialize)]
struct ExpectationsFile {
    name: String,
    /// Path to the fixture corpus, relative to repo root. Reuses `fixtures/corpus/*`
    /// directories so we don't duplicate sample TypeScript files.
    corpus: String,
    /// "implemented" or "scaffold". Scaffold tasks emit a skipped report — they
    /// exist so the directory layout is in place ready for follow-up PRs.
    #[serde(default = "default_status")]
    status: String,
    /// Whether to include this task in `--fast` runs (CI subset).
    #[serde(default, rename = "fastSubset")]
    fast_subset: bool,
    /// Steps to execute, in order.
    #[serde(default)]
    steps: Vec<StepSpec>,
    /// Pass/fail rules to evaluate after all steps complete.
    #[serde(default)]
    expectations: Vec<ExpectationSpec>,
}

fn default_status() -> String {
    "implemented".into()
}

#[derive(Debug, Clone, Deserialize)]
struct StepSpec {
    name: String,
    /// Logical tool name — "Search" or "Read" (no `relaywash__` prefix in the
    /// fixture file; the runner maps it).
    tool: String,
    /// Args passed to the tool handler. The runner injects `cwd` automatically
    /// based on the corpus directory; explicit `cwd` in the spec overrides.
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ExpectationSpec {
    /// At least one of the named files appears in the top N hits of a step.
    FileInTopResults {
        name: String,
        step: String,
        files: Vec<String>,
        #[serde(rename = "topN", default = "default_top_n")]
        top_n: usize,
    },
    /// The Read step returned content covering at least one of the listed line
    /// ranges (inclusive). Used to check that a follow-up `mode: "range"` read
    /// landed on the right function body.
    LineRangeRead {
        name: String,
        step: String,
        /// Each entry is `[start, end]`.
        ranges: Vec<[u32; 2]>,
    },
    /// The Search step produced at least this many hits. Catches a regression
    /// where a tuning change silently drops match results.
    MinHitCount {
        name: String,
        step: String,
        min: usize,
    },
    /// The Search step produced no more than this many hits. Catches a regression
    /// where caps fail to bound noisy result sets.
    MaxHitCount {
        name: String,
        step: String,
        max: usize,
    },
    /// Total response bytes across all calls in the task stay below this limit.
    /// Useful as a soft budget: a baseline-only tripwire that flags growth.
    MaxTotalBytes {
        name: String,
        max: u64,
    },
}

fn default_top_n() -> usize {
    5
}

/// Run a whole suite. Discovers tasks under `fixtures/bench/`, filters by name or
/// fast subset, and aggregates everything into one `SuiteReport`.
pub fn run_suite(opts: &RunOptions) -> Result<SuiteReport> {
    let bench_root = opts.repo_root.join("fixtures").join("bench");
    let mut task_dirs: Vec<PathBuf> = std::fs::read_dir(&bench_root)
        .with_context(|| format!("reading {}", bench_root.display()))?
        .filter_map(|e| e.ok().map(|d| d.path()))
        .filter(|p| p.is_dir())
        .collect();
    task_dirs.sort();

    let want: HashSet<&str> = opts.tasks.iter().map(String::as_str).collect();
    let tools = tools::all();
    let mut tasks: Vec<TaskReport> = Vec::new();
    let mut totals = RunReport::default();
    let mut totals_for_repeats: HashSet<String> = HashSet::new();

    for dir in task_dirs {
        let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if !want.is_empty() && !want.contains(name.as_str()) {
            continue;
        }
        let task = match run_task(&opts.repo_root, &dir, &tools, opts.fast_only) {
            Ok(Some(task)) => task,
            Ok(None) => continue, // filtered out by --fast
            Err(e) => return Err(e.context(format!("running task {name}"))),
        };
        // Roll suite-wide repeated-call detection up. A repeat in any task counts.
        for call in &task.calls {
            let key = canonical_call_key(&call.tool, &call.args);
            if !totals_for_repeats.insert(key) {
                totals.any_repeated_call = true;
            }
        }
        totals.merge(&task.totals);
        tasks.push(task);
    }

    Ok(SuiteReport {
        schema_version: BENCH_SCHEMA_VERSION,
        label: opts.label.clone(),
        tasks,
        totals,
    })
}

/// Run a single task. Returns `Ok(None)` if the task is filtered out by `fast_only`.
pub fn run_task(
    repo_root: &Path,
    task_dir: &Path,
    tools: &[Tool],
    fast_only: bool,
) -> Result<Option<TaskReport>> {
    let exp_path = task_dir.join("expectations.json");
    let raw = std::fs::read_to_string(&exp_path)
        .with_context(|| format!("reading {}", exp_path.display()))?;
    let spec: ExpectationsFile = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {}", exp_path.display()))?;

    if fast_only && !spec.fast_subset {
        return Ok(None);
    }

    // Scaffold tasks: emit a deterministic skipped TaskReport so the suite still has
    // a row for them — makes it obvious from the report which tasks are pending.
    if spec.status != "implemented" {
        return Ok(Some(TaskReport {
            name: spec.name,
            calls: Vec::new(),
            expectations: vec![ExpectationOutcome {
                name: "scaffold-only".into(),
                passed: true,
                detail: "task is scaffold-only; implementation in follow-up PR".into(),
            }],
            totals: RunReport::default(),
            passed: true,
        }));
    }

    let corpus_path = repo_root.join(&spec.corpus);
    if !corpus_path.exists() {
        return Err(anyhow!(
            "corpus path {} does not exist (referenced from {})",
            corpus_path.display(),
            exp_path.display()
        ));
    }

    // Drive the steps. Session id is fixed so the per-process state cache (Read's
    // mtime cache, last-searched-symbol) is consistent across runs of the same task.
    let ctx = ToolContext { session_id: Some(format!("bench-{}", spec.name)) };
    // Reset the global state between tasks so an earlier task's read cache doesn't
    // make a later task's first Read return empty content.
    #[cfg(test)]
    crate::state::reset();
    // Reset symbol state without depending on cfg(test). Searching with an empty
    // symbol clears `last_search_symbol`.
    crate::state::note_searched_symbol(None);

    let mut calls: Vec<CallRecord> = Vec::with_capacity(spec.steps.len());
    let mut step_outputs: Vec<(String, Value)> = Vec::with_capacity(spec.steps.len());
    let mut seen_args: HashSet<String> = HashSet::new();
    let mut any_repeated = false;

    for step in &spec.steps {
        let tool_name = match step.tool.as_str() {
            "Search" => "relaywash__Search",
            "Read" => "relaywash__Read",
            other => return Err(anyhow!("unsupported tool in expectations: {other}")),
        };
        let tool = tools
            .iter()
            .find(|t| t.name == tool_name)
            .ok_or_else(|| anyhow!("tool {tool_name} not registered"))?;

        let mut args = step.args.clone();
        // Inject cwd if not specified. For Search, cwd is an argument; for Read,
        // the path is taken relative to the corpus root.
        if step.tool == "Search" && args.get("cwd").is_none() {
            args.as_object_mut()
                .ok_or_else(|| anyhow!("step args must be an object"))?
                .insert("cwd".into(), json!(corpus_path.to_string_lossy()));
        } else if step.tool == "Read"
            && let Some(p) = args.get("path").and_then(|v| v.as_str())
        {
            // Resolve path relative to the corpus directory unless absolute.
            let p = Path::new(p);
            let resolved = if p.is_absolute() {
                p.to_path_buf()
            } else {
                corpus_path.join(p)
            };
            args.as_object_mut()
                .ok_or_else(|| anyhow!("step args must be an object"))?
                .insert("path".into(), json!(resolved.to_string_lossy()));
        }

        let key = canonical_call_key(tool_name, &args);
        if !seen_args.insert(key) {
            any_repeated = true;
        }

        let outcome = (tool.handler)(&args, &ctx);
        let (formatted, structured, error) = match outcome {
            Ok(out) => {
                let formatted = format_tool_result(&out);
                let structured = formatted
                    .get("structuredContent")
                    .cloned()
                    .unwrap_or(Value::Null);
                (formatted, structured, false)
            }
            Err(e) => (
                json!({
                    "content": [{"type": "text", "text": e.to_string()}],
                    "isError": true,
                }),
                Value::Null,
                true,
            ),
        };

        let response_bytes = extract_response_bytes(&formatted);
        let baseline_bytes = extract_baseline_bytes(&formatted);
        let cap_hit = detect_cap_hit(&structured);

        calls.push(CallRecord {
            step: step.name.clone(),
            tool: tool_name.into(),
            args: step.args.clone(),
            response_bytes,
            response_tokens: estimate_tokens(response_bytes),
            baseline_bytes,
            cap_hit,
            error,
        });
        step_outputs.push((step.name.clone(), structured));
    }

    let expectations: Vec<ExpectationOutcome> = spec
        .expectations
        .iter()
        .map(|e| evaluate_expectation(e, &step_outputs, &calls))
        .collect();

    let mut totals = RunReport::default();
    for call in &calls {
        totals.fold_call(call);
    }
    totals.any_repeated_call = any_repeated;

    let passed = expectations.iter().all(|e| e.passed) && !calls.iter().any(|c| c.error);

    Ok(Some(TaskReport {
        name: spec.name,
        calls,
        expectations,
        totals,
        passed,
    }))
}

/// Pull `_meta.responseBytes` out of the formatted result. Falls back to the
/// compact-JSON length of the text content block when no Meta is attached
/// (defensive — every tool we exercise here today attaches one).
fn extract_response_bytes(formatted: &Value) -> u64 {
    if let Some(n) = formatted
        .get("structuredContent")
        .and_then(|v| v.get("_meta"))
        .and_then(|v| v.get("responseBytes"))
        .and_then(|v| v.as_u64())
    {
        return n;
    }
    formatted
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("text"))
        .and_then(|v| v.as_str())
        .map(|s| s.len() as u64)
        .unwrap_or(0)
}

fn extract_baseline_bytes(formatted: &Value) -> Option<u64> {
    formatted
        .get("structuredContent")
        .and_then(|v| v.get("_meta"))
        .and_then(|v| v.get("baselineBytes"))
        .and_then(|v| v.as_u64())
}

/// Heuristic cap detection. Mirrors the actual flags the tools emit so the
/// signal stays accurate as those tools grow more cap surfaces.
fn detect_cap_hit(structured: &Value) -> bool {
    structured.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false)
        || structured
            .get("skippedTruncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}

fn canonical_call_key(tool: &str, args: &Value) -> String {
    // Sort object keys for stable hashing. Skip `cwd` — it's runner-injected and
    // would otherwise cause every cross-task call to look unique.
    let stripped = match args {
        Value::Object(map) => {
            let mut m: serde_json::Map<String, Value> = map.clone();
            m.remove("cwd");
            // Sort by key for stable serialization.
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for k in keys {
                sorted.insert(k.clone(), m[k].clone());
            }
            Value::Object(sorted)
        }
        other => other.clone(),
    };
    format!("{tool}::{}", serde_json::to_string(&stripped).unwrap_or_default())
}

fn evaluate_expectation(
    spec: &ExpectationSpec,
    outputs: &[(String, Value)],
    calls: &[CallRecord],
) -> ExpectationOutcome {
    match spec {
        ExpectationSpec::FileInTopResults { name, step, files, top_n } => {
            let out = outputs.iter().find(|(s, _)| s == step);
            let Some((_, val)) = out else {
                return fail(name, format!("step {step} not found in run"));
            };
            let results = val.get("results").and_then(|v| v.as_array());
            let Some(results) = results else {
                return fail(name, format!("step {step} did not return `results`"));
            };
            let top: Vec<&str> = results
                .iter()
                .take(*top_n)
                .filter_map(|h| h.get("path").and_then(|v| v.as_str()))
                .collect();
            if files.iter().any(|f| top.contains(&f.as_str())) {
                pass(name)
            } else {
                fail(
                    name,
                    format!(
                        "none of {files:?} appeared in top {top_n}: got {top:?}",
                    ),
                )
            }
        }
        ExpectationSpec::LineRangeRead { name, step, ranges } => {
            let call = calls.iter().find(|c| c.step == *step);
            let Some(call) = call else {
                return fail(name, format!("call for step {step} not found"));
            };
            let arg_range = call
                .args
                .get("range")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    if arr.len() == 2 {
                        Some([arr[0].as_u64()? as u32, arr[1].as_u64()? as u32])
                    } else {
                        None
                    }
                });
            let Some([s, e]) = arg_range else {
                return fail(name, format!("step {step} args missing [start, end] range"));
            };
            if ranges.iter().any(|[ws, we]| s >= *ws && e <= *we) {
                pass(name)
            } else {
                fail(
                    name,
                    format!(
                        "read range [{s}, {e}] not contained in any expected window {ranges:?}"
                    ),
                )
            }
        }
        ExpectationSpec::MinHitCount { name, step, min } => {
            let out = outputs.iter().find(|(s, _)| s == step);
            let Some((_, val)) = out else {
                return fail(name, format!("step {step} not found"));
            };
            let count = val
                .get("results")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if count >= *min {
                pass(name)
            } else {
                fail(name, format!("got {count} hits, expected >= {min}"))
            }
        }
        ExpectationSpec::MaxHitCount { name, step, max } => {
            let out = outputs.iter().find(|(s, _)| s == step);
            let Some((_, val)) = out else {
                return fail(name, format!("step {step} not found"));
            };
            let count = val
                .get("results")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if count <= *max {
                pass(name)
            } else {
                fail(name, format!("got {count} hits, expected <= {max}"))
            }
        }
        ExpectationSpec::MaxTotalBytes { name, max } => {
            let total: u64 = calls.iter().map(|c| c.response_bytes).sum();
            if total <= *max {
                pass(name)
            } else {
                fail(name, format!("total bytes {total} > budget {max}"))
            }
        }
    }
}

fn pass(name: &str) -> ExpectationOutcome {
    ExpectationOutcome { name: name.into(), passed: true, detail: String::new() }
}

fn fail(name: &str, detail: impl Into<String>) -> ExpectationOutcome {
    ExpectationOutcome { name: name.into(), passed: false, detail: detail.into() }
}

/// Re-export so callers can sanity-check the tool meta version they're recording
/// against. Not currently used by run_task but kept here for downstream consumers.
#[allow(dead_code)]
pub const META_SCHEMA_VERSION: u32 = SCHEMA_VERSION;

#[cfg(test)]
mod tests {
    use super::*;

    /// Locate the workspace root from the test process — `CARGO_MANIFEST_DIR` resolves
    /// to `crates/wash`, two levels under the repo root.
    fn repo_root() -> PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root reachable from CARGO_MANIFEST_DIR")
            .to_path_buf()
    }

    #[test]
    fn canonical_call_key_ignores_cwd_and_orders_keys() {
        let a = serde_json::json!({"symbol":"foo","cwd":"/a","contextLines":2});
        let b = serde_json::json!({"contextLines":2,"symbol":"foo","cwd":"/b"});
        assert_eq!(
            canonical_call_key("relaywash__Search", &a),
            canonical_call_key("relaywash__Search", &b),
        );
    }

    #[test]
    fn find_and_read_task_runs_end_to_end() {
        let root = repo_root();
        let tools = crate::tools::all();
        let task_dir = root.join("fixtures/bench/find-and-read");
        let task = run_task(&root, &task_dir, &tools, false)
            .expect("runner succeeds")
            .expect("task ran (not filtered)");
        assert!(task.passed, "task failed: {task:#?}");
        assert_eq!(task.calls.len(), 2);
        assert!(task.calls.iter().all(|c| c.response_bytes > 0));
        assert!(task.calls.iter().all(|c| c.response_tokens > 0));
        assert!(task.expectations.iter().all(|e| e.passed));
    }

    #[test]
    fn fast_filter_skips_scaffold_tasks() {
        let root = repo_root();
        let tools = crate::tools::all();
        let scaffold = root.join("fixtures/bench/explore-subsystem");
        let task = run_task(&root, &scaffold, &tools, true).expect("runner succeeds");
        assert!(task.is_none(), "scaffold task should be filtered out of --fast");
    }

    #[test]
    fn scaffold_task_runs_as_skipped_when_not_filtered() {
        let root = repo_root();
        let tools = crate::tools::all();
        let scaffold = root.join("fixtures/bench/explore-subsystem");
        let task = run_task(&root, &scaffold, &tools, false)
            .expect("runner succeeds")
            .expect("scaffold included");
        assert!(task.passed);
        assert_eq!(task.calls.len(), 0);
        assert_eq!(task.expectations[0].name, "scaffold-only");
    }
}
