//! Evaluation harness: runs fixed tasks against fixture corpora and reports output
//! size, call count, hit quality, and regression status. Drives the same code paths
//! the MCP server uses so metrics line up with real agent usage.
//!
//! Layout:
//! - `run`     — execute a single benchmark task end-to-end, collect metrics.
//! - `compare` — read two saved run reports and print deltas; flag regressions.
//! - `report`  — serializable record of a single task run (or full suite).
//!
//! The CLI surface lives in `crates/wash/src/main.rs` under `wash bench`.

pub mod compare;
pub mod report;
pub mod run;

pub use compare::{CompareOutcome, CompareOptions, compare};
pub use report::{CallRecord, ExpectationOutcome, RunReport, SuiteReport, TaskReport};
pub use run::{RunOptions, run_suite, run_task};
