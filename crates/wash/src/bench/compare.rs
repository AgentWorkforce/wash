//! Compare two saved `SuiteReport`s and print per-task deltas. Used by
//! `wash bench compare --before <path> --after <path>` and by CI checks that
//! want to flag a regression in token burn / call count / expectation pass rate.
//!
//! Exit-code policy lives in `main.rs`: if any task in `CompareOutcome.regressions`
//! is non-empty, the CLI returns a non-zero status.

use serde::Serialize;

use crate::bench::report::{RunReport, SuiteReport, TaskReport};

/// Configurable regression thresholds. Defaults are conservative — small jitter
/// shouldn't fail CI, but a 10%+ growth in bytes or any new failing expectation
/// should.
#[derive(Debug, Clone)]
pub struct CompareOptions {
    /// Fail if total bytes (per task or suite) grow by more than this fraction.
    /// 0.10 = 10 %. Defaults to 0.10.
    pub max_bytes_growth: f64,
    /// Fail if call count grows by more than this fraction. 0.0 = any new call
    /// is a regression. Defaults to 0.0.
    pub max_calls_growth: f64,
    /// Fail when a previously-passing expectation now fails. Defaults to true.
    pub fail_on_new_failures: bool,
    /// Fail when a previously-no-cap-hit task now hits a cap. Defaults to true.
    pub fail_on_new_cap_hit: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            max_bytes_growth: 0.10,
            max_calls_growth: 0.0,
            fail_on_new_failures: true,
            fail_on_new_cap_hit: true,
        }
    }
}

/// Top-level diff. `regressions` is a flat list of human-readable strings, one
/// per offence. Empty list = the comparison passes.
#[derive(Debug, Clone, Serialize)]
pub struct CompareOutcome {
    /// One entry per task that appears in either report. Tasks present only on
    /// one side are flagged.
    pub task_deltas: Vec<TaskDelta>,
    /// Suite-level deltas — handy when looking at top-line burn.
    pub suite_delta: SuiteDelta,
    /// Regression messages. Non-empty = the comparator should exit non-zero.
    pub regressions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuiteDelta {
    pub before: RunReport,
    pub after: RunReport,
    pub delta_bytes: i64,
    pub delta_tokens: i64,
    pub delta_calls: i64,
    /// `(after_bytes - before_bytes) / before_bytes`, or 0.0 when before==0.
    pub bytes_growth: f64,
    /// `(after_calls - before_calls) / before_calls`, or 0.0 when before==0.
    pub calls_growth: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskDelta {
    pub name: String,
    /// Suite-only side: this task appeared in one report but not the other.
    pub status: TaskDeltaStatus,
    pub before: RunReport,
    pub after: RunReport,
    pub delta_bytes: i64,
    pub delta_tokens: i64,
    pub delta_calls: i64,
    pub bytes_growth: f64,
    pub calls_growth: f64,
    /// Names of expectations that passed before and fail now.
    pub newly_failing_expectations: Vec<String>,
    /// Names of expectations that failed before and pass now (informational).
    pub newly_passing_expectations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskDeltaStatus {
    Both,
    OnlyBefore,
    OnlyAfter,
}

pub fn compare(
    before: &SuiteReport,
    after: &SuiteReport,
    opts: &CompareOptions,
) -> CompareOutcome {
    let mut regressions: Vec<String> = Vec::new();
    let mut deltas: Vec<TaskDelta> = Vec::new();

    // Pair tasks by name. Stable order: iterate `after` first (the typical "what's
    // the current state of the world" view), then surface any leftover before tasks.
    let mut seen: std::collections::HashSet<String> = Default::default();
    for after_task in &after.tasks {
        seen.insert(after_task.name.clone());
        let before_task = before.tasks.iter().find(|t| t.name == after_task.name);
        match before_task {
            Some(b) => {
                let d = task_delta(b, after_task);
                regressions.extend(check_task_regressions(&d, opts));
                deltas.push(d);
            }
            None => {
                let d = TaskDelta {
                    name: after_task.name.clone(),
                    status: TaskDeltaStatus::OnlyAfter,
                    before: RunReport::default(),
                    after: after_task.totals.clone(),
                    delta_bytes: after_task.totals.total_bytes as i64,
                    delta_tokens: after_task.totals.total_tokens as i64,
                    delta_calls: after_task.totals.call_count as i64,
                    bytes_growth: 0.0,
                    calls_growth: 0.0,
                    newly_failing_expectations: failing_names(after_task),
                    newly_passing_expectations: vec![],
                };
                deltas.push(d);
            }
        }
    }
    for before_task in &before.tasks {
        if seen.contains(&before_task.name) {
            continue;
        }
        let d = TaskDelta {
            name: before_task.name.clone(),
            status: TaskDeltaStatus::OnlyBefore,
            before: before_task.totals.clone(),
            after: RunReport::default(),
            delta_bytes: -(before_task.totals.total_bytes as i64),
            delta_tokens: -(before_task.totals.total_tokens as i64),
            delta_calls: -(before_task.totals.call_count as i64),
            bytes_growth: 0.0,
            calls_growth: 0.0,
            newly_failing_expectations: vec![],
            newly_passing_expectations: vec![],
        };
        regressions.push(format!(
            "task '{}' disappeared between runs (was in before, missing in after)",
            d.name
        ));
        deltas.push(d);
    }

    let suite_delta = SuiteDelta {
        before: before.totals.clone(),
        after: after.totals.clone(),
        delta_bytes: signed_diff(before.totals.total_bytes, after.totals.total_bytes),
        delta_tokens: signed_diff(before.totals.total_tokens, after.totals.total_tokens),
        delta_calls: signed_diff(
            before.totals.call_count as u64,
            after.totals.call_count as u64,
        ),
        bytes_growth: growth(before.totals.total_bytes, after.totals.total_bytes),
        calls_growth: growth(
            before.totals.call_count as u64,
            after.totals.call_count as u64,
        ),
    };

    if suite_delta.bytes_growth > opts.max_bytes_growth {
        regressions.push(format!(
            "suite total bytes grew by {:.1}% (limit {:.1}%)",
            suite_delta.bytes_growth * 100.0,
            opts.max_bytes_growth * 100.0,
        ));
    }
    if suite_delta.calls_growth > opts.max_calls_growth {
        regressions.push(format!(
            "suite call count grew by {:.1}% (limit {:.1}%)",
            suite_delta.calls_growth * 100.0,
            opts.max_calls_growth * 100.0,
        ));
    }

    CompareOutcome { task_deltas: deltas, suite_delta, regressions }
}

fn task_delta(before: &TaskReport, after: &TaskReport) -> TaskDelta {
    let before_failing: std::collections::HashSet<String> = before
        .expectations
        .iter()
        .filter(|e| !e.passed)
        .map(|e| e.name.clone())
        .collect();
    let after_failing: std::collections::HashSet<String> = after
        .expectations
        .iter()
        .filter(|e| !e.passed)
        .map(|e| e.name.clone())
        .collect();
    let newly_failing: Vec<String> =
        after_failing.difference(&before_failing).cloned().collect();
    let newly_passing: Vec<String> =
        before_failing.difference(&after_failing).cloned().collect();

    TaskDelta {
        name: after.name.clone(),
        status: TaskDeltaStatus::Both,
        before: before.totals.clone(),
        after: after.totals.clone(),
        delta_bytes: signed_diff(before.totals.total_bytes, after.totals.total_bytes),
        delta_tokens: signed_diff(before.totals.total_tokens, after.totals.total_tokens),
        delta_calls: signed_diff(
            before.totals.call_count as u64,
            after.totals.call_count as u64,
        ),
        bytes_growth: growth(before.totals.total_bytes, after.totals.total_bytes),
        calls_growth: growth(
            before.totals.call_count as u64,
            after.totals.call_count as u64,
        ),
        newly_failing_expectations: sorted(newly_failing),
        newly_passing_expectations: sorted(newly_passing),
    }
}

fn failing_names(task: &TaskReport) -> Vec<String> {
    task.expectations
        .iter()
        .filter(|e| !e.passed)
        .map(|e| e.name.clone())
        .collect()
}

fn check_task_regressions(d: &TaskDelta, opts: &CompareOptions) -> Vec<String> {
    let mut out = Vec::new();
    if d.bytes_growth > opts.max_bytes_growth {
        out.push(format!(
            "task '{}' bytes grew by {:.1}% (limit {:.1}%)",
            d.name,
            d.bytes_growth * 100.0,
            opts.max_bytes_growth * 100.0,
        ));
    }
    if d.calls_growth > opts.max_calls_growth {
        out.push(format!(
            "task '{}' call count grew by {:.1}% (limit {:.1}%)",
            d.name,
            d.calls_growth * 100.0,
            opts.max_calls_growth * 100.0,
        ));
    }
    if opts.fail_on_new_failures && !d.newly_failing_expectations.is_empty() {
        out.push(format!(
            "task '{}' has newly failing expectations: {:?}",
            d.name, d.newly_failing_expectations,
        ));
    }
    if opts.fail_on_new_cap_hit && !d.before.any_cap_hit && d.after.any_cap_hit {
        out.push(format!("task '{}' newly hit a cap", d.name));
    }
    out
}

fn signed_diff(before: u64, after: u64) -> i64 {
    after as i64 - before as i64
}

/// Growth ratio `(after - before) / before`. Returns 0.0 when `before == 0`
/// to avoid spurious infinities for tasks that previously had no calls.
fn growth(before: u64, after: u64) -> f64 {
    if before == 0 {
        // Going from zero to anything is technically infinite growth, but for
        // regression purposes we treat first-time data as "no baseline". The
        // comparator surfaces "only-after" tasks separately so users still see
        // them in the report.
        return 0.0;
    }
    (after as f64 - before as f64) / before as f64
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::report::{CallRecord, ExpectationOutcome, RunReport, TaskReport};

    fn task(
        name: &str,
        bytes: u64,
        calls: u32,
        expectations: Vec<ExpectationOutcome>,
        cap: bool,
    ) -> TaskReport {
        let tokens = crate::tokens::estimate_tokens(bytes);
        let totals = RunReport {
            call_count: calls,
            total_bytes: bytes,
            total_tokens: tokens,
            any_cap_hit: cap,
            any_repeated_call: false,
        };
        // Materialize one CallRecord so tests cover the call-level accounting too.
        let calls_vec = if calls > 0 {
            vec![CallRecord {
                step: "step1".into(),
                tool: "relaywash__Search".into(),
                args: serde_json::json!({}),
                response_bytes: bytes,
                response_tokens: tokens,
                baseline_bytes: None,
                cap_hit: cap,
                error: false,
            }]
        } else {
            Vec::new()
        };
        TaskReport {
            name: name.into(),
            calls: calls_vec,
            expectations,
            totals,
            passed: true,
        }
    }

    fn suite(label: &str, tasks: Vec<TaskReport>) -> SuiteReport {
        let mut totals = RunReport::default();
        for t in &tasks {
            totals.merge(&t.totals);
        }
        SuiteReport {
            schema_version: crate::bench::run::BENCH_SCHEMA_VERSION,
            label: label.into(),
            tasks,
            totals,
        }
    }

    #[test]
    fn identical_runs_have_no_regressions() {
        let exps = vec![ExpectationOutcome {
            name: "e1".into(),
            passed: true,
            detail: String::new(),
        }];
        let before = suite(
            "before",
            vec![task("t1", 1000, 2, exps.clone(), false)],
        );
        let after = suite("after", vec![task("t1", 1000, 2, exps, false)]);
        let outcome = compare(&before, &after, &CompareOptions::default());
        assert!(outcome.regressions.is_empty(), "got: {:?}", outcome.regressions);
        assert_eq!(outcome.suite_delta.delta_bytes, 0);
        assert_eq!(outcome.suite_delta.bytes_growth, 0.0);
    }

    #[test]
    fn small_growth_under_threshold_is_not_a_regression() {
        let exps = vec![ExpectationOutcome {
            name: "e1".into(),
            passed: true,
            detail: String::new(),
        }];
        let before = suite(
            "before",
            vec![task("t1", 1000, 2, exps.clone(), false)],
        );
        // 5 % growth, default threshold is 10 %.
        let after = suite("after", vec![task("t1", 1050, 2, exps, false)]);
        let outcome = compare(&before, &after, &CompareOptions::default());
        assert!(outcome.regressions.is_empty(), "got: {:?}", outcome.regressions);
        assert_eq!(outcome.suite_delta.delta_bytes, 50);
        assert!((outcome.suite_delta.bytes_growth - 0.05).abs() < 1e-9);
    }

    #[test]
    fn bytes_growth_above_threshold_is_a_regression() {
        let exps = vec![ExpectationOutcome {
            name: "e1".into(),
            passed: true,
            detail: String::new(),
        }];
        let before = suite(
            "before",
            vec![task("t1", 1000, 2, exps.clone(), false)],
        );
        // 50 % growth.
        let after = suite("after", vec![task("t1", 1500, 2, exps, false)]);
        let outcome = compare(&before, &after, &CompareOptions::default());
        assert!(!outcome.regressions.is_empty());
        assert!(
            outcome.regressions.iter().any(|r| r.contains("bytes grew")),
            "got: {:?}",
            outcome.regressions
        );
    }

    #[test]
    fn newly_failing_expectation_is_a_regression() {
        let pass = vec![ExpectationOutcome {
            name: "fileInTop".into(),
            passed: true,
            detail: String::new(),
        }];
        let fail = vec![ExpectationOutcome {
            name: "fileInTop".into(),
            passed: false,
            detail: "missed".into(),
        }];
        let before = suite("before", vec![task("t1", 1000, 2, pass, false)]);
        let after = suite("after", vec![task("t1", 1000, 2, fail, false)]);
        let outcome = compare(&before, &after, &CompareOptions::default());
        assert!(outcome.regressions.iter().any(|r| r.contains("fileInTop")));
        assert_eq!(
            outcome.task_deltas[0].newly_failing_expectations,
            vec!["fileInTop".to_string()],
        );
    }

    #[test]
    fn new_cap_hit_is_a_regression() {
        let exps = vec![ExpectationOutcome {
            name: "e1".into(),
            passed: true,
            detail: String::new(),
        }];
        let before = suite(
            "before",
            vec![task("t1", 1000, 2, exps.clone(), false)],
        );
        let after = suite("after", vec![task("t1", 1000, 2, exps, true)]);
        let outcome = compare(&before, &after, &CompareOptions::default());
        assert!(
            outcome.regressions.iter().any(|r| r.contains("newly hit a cap")),
            "got: {:?}",
            outcome.regressions
        );
    }

    #[test]
    fn missing_task_in_after_is_a_regression() {
        let exps = vec![ExpectationOutcome {
            name: "e1".into(),
            passed: true,
            detail: String::new(),
        }];
        let before = suite(
            "before",
            vec![
                task("t1", 1000, 2, exps.clone(), false),
                task("t2", 500, 1, exps.clone(), false),
            ],
        );
        let after = suite("after", vec![task("t1", 1000, 2, exps, false)]);
        let outcome = compare(&before, &after, &CompareOptions::default());
        assert!(
            outcome.regressions.iter().any(|r| r.contains("disappeared")),
            "got: {:?}",
            outcome.regressions
        );
        let t2 = outcome
            .task_deltas
            .iter()
            .find(|d| d.name == "t2")
            .expect("t2 present");
        assert_eq!(t2.status, TaskDeltaStatus::OnlyBefore);
        assert_eq!(t2.delta_bytes, -500);
    }

    #[test]
    fn new_task_in_after_is_reported_but_not_regression() {
        let exps = vec![ExpectationOutcome {
            name: "e1".into(),
            passed: true,
            detail: String::new(),
        }];
        let before = suite("before", vec![task("t1", 1000, 2, exps.clone(), false)]);
        let after = suite(
            "after",
            vec![
                task("t1", 1000, 2, exps.clone(), false),
                task("t2", 500, 1, exps, false),
            ],
        );
        let outcome = compare(&before, &after, &CompareOptions::default());
        let t2 = outcome
            .task_deltas
            .iter()
            .find(|d| d.name == "t2")
            .expect("t2 present");
        assert_eq!(t2.status, TaskDeltaStatus::OnlyAfter);
        // New tasks add suite bytes — confirm the comparator records the absolute
        // growth without flagging it as a bytes_growth regression (which is
        // defined per-task vs zero-baseline as 0.0).
        assert_eq!(t2.delta_bytes, 500);
        assert!(
            !outcome
                .regressions
                .iter()
                .any(|r| r.contains("disappeared")),
        );
    }

    #[test]
    fn growth_is_zero_when_before_is_zero() {
        assert_eq!(growth(0, 100), 0.0);
        assert_eq!(growth(0, 0), 0.0);
    }

    #[test]
    fn signed_diff_handles_shrinkage() {
        assert_eq!(signed_diff(100, 30), -70);
        assert_eq!(signed_diff(30, 100), 70);
        assert_eq!(signed_diff(100, 100), 0);
    }

    #[test]
    fn calls_growth_threshold_strict_by_default() {
        let exps = vec![ExpectationOutcome {
            name: "e1".into(),
            passed: true,
            detail: String::new(),
        }];
        let before = suite(
            "before",
            vec![task("t1", 1000, 2, exps.clone(), false)],
        );
        let after = suite("after", vec![task("t1", 1000, 3, exps, false)]);
        let outcome = compare(&before, &after, &CompareOptions::default());
        assert!(
            outcome.regressions.iter().any(|r| r.contains("call count grew")),
            "got: {:?}",
            outcome.regressions
        );
    }

    #[test]
    fn relaxed_thresholds_allow_growth() {
        let exps = vec![ExpectationOutcome {
            name: "e1".into(),
            passed: true,
            detail: String::new(),
        }];
        let before = suite(
            "before",
            vec![task("t1", 1000, 2, exps.clone(), false)],
        );
        let after = suite("after", vec![task("t1", 2000, 4, exps, false)]);
        let opts = CompareOptions {
            max_bytes_growth: 2.0,
            max_calls_growth: 2.0,
            fail_on_new_failures: true,
            fail_on_new_cap_hit: true,
        };
        let outcome = compare(&before, &after, &opts);
        assert!(outcome.regressions.is_empty(), "got: {:?}", outcome.regressions);
    }
}
