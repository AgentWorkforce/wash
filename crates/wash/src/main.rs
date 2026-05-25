use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "wash", version, about = "relaywash — clean agent tool output, lower token burn")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP stdio server (default action).
    Mcp,
    /// Run a hook handler. `kind` matches the entry registered in hooks/hooks.json.
    Hook {
        kind: String,
    },
    /// Print the relayburn savings summary for a session (or all sessions).
    Savings {
        /// Session id. Omit to aggregate across every session in the ledger.
        #[arg(long)]
        session: Option<String>,
    },
    /// Compare replacement vs vanilla bytes against the fixture corpus. Reserved for later PR.
    BurnCompare,
    /// Evaluation harness: run benchmark tasks against fixture corpora or compare two saved runs.
    Bench {
        #[command(subcommand)]
        action: BenchCmd,
    },
}

#[derive(Subcommand)]
enum BenchCmd {
    /// Execute the benchmark suite and write a JSON report.
    Run {
        /// Repo root. Defaults to autodetected (walks up looking for `fixtures/bench/`).
        #[arg(long)]
        repo_root: Option<PathBuf>,
        /// Restrict to these task names. Repeatable. Omit to run all tasks.
        #[arg(long = "task")]
        tasks: Vec<String>,
        /// CI subset: only run tasks tagged `"fastSubset": true`.
        ///
        /// CI hook: invoke `wash bench run --fast --out target/bench/ci.json` and then
        /// `wash bench compare --before <baseline> --after target/bench/ci.json` in a
        /// downstream job. Keep the fast subset under ~30 seconds total.
        #[arg(long)]
        fast: bool,
        /// Where to write the JSON report. Omit to print to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Free-form label embedded in the report (e.g. a commit SHA).
        #[arg(long, default_value = "local")]
        label: String,
    },
    /// Compare two saved suite reports and exit non-zero on regression.
    Compare {
        #[arg(long)]
        before: PathBuf,
        #[arg(long)]
        after: PathBuf,
        /// Max allowed total-bytes growth fraction (0.10 = 10 %). Default 0.10.
        #[arg(long)]
        max_bytes_growth: Option<f64>,
        /// Max allowed call-count growth fraction. Default 0.0 (strict).
        #[arg(long)]
        max_calls_growth: Option<f64>,
        /// Print full JSON outcome to stdout in addition to the human summary.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Mcp) {
        Command::Mcp => wash::mcp::serve(),
        Command::Hook { kind } => wash::hooks::run(&kind),
        Command::Savings { session } => wash::savings::run(session.as_deref()),
        Command::BurnCompare => {
            eprintln!("wash burn-compare: not implemented yet (later PR)");
            std::process::exit(2);
        }
        Command::Bench { action } => run_bench(action),
    }
}

fn run_bench(action: BenchCmd) -> Result<()> {
    match action {
        BenchCmd::Run { repo_root, tasks, fast, out, label } => {
            let root = resolve_repo_root(repo_root)?;
            let report = wash::bench::run_suite(&wash::bench::RunOptions {
                repo_root: root,
                tasks,
                fast_only: fast,
                label,
            })?;
            let body = serde_json::to_string_pretty(&report)? + "\n";
            match out {
                Some(path) => {
                    if let Some(parent) = path.parent()
                        && !parent.as_os_str().is_empty()
                    {
                        std::fs::create_dir_all(parent)
                            .with_context(|| format!("creating {}", parent.display()))?;
                    }
                    std::fs::write(&path, body)
                        .with_context(|| format!("writing {}", path.display()))?;
                    print_human_summary(&report);
                }
                None => print!("{body}"),
            }
            // Nonzero on any failing task so a developer running locally without
            // `compare` still sees an obvious failure code.
            if report.tasks.iter().any(|t| !t.passed) {
                std::process::exit(1);
            }
            Ok(())
        }
        BenchCmd::Compare { before, after, max_bytes_growth, max_calls_growth, json } => {
            let before_raw = std::fs::read_to_string(&before)
                .with_context(|| format!("reading {}", before.display()))?;
            let after_raw = std::fs::read_to_string(&after)
                .with_context(|| format!("reading {}", after.display()))?;
            let before_report: wash::bench::SuiteReport = serde_json::from_str(&before_raw)?;
            let after_report: wash::bench::SuiteReport = serde_json::from_str(&after_raw)?;
            let mut opts = wash::bench::CompareOptions::default();
            if let Some(v) = max_bytes_growth {
                opts.max_bytes_growth = v;
            }
            if let Some(v) = max_calls_growth {
                opts.max_calls_growth = v;
            }
            let outcome = wash::bench::compare(&before_report, &after_report, &opts);
            print_compare_summary(&before_report, &after_report, &outcome);
            if json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            }
            if !outcome.regressions.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

/// Autodetect the repo root by walking up from CWD looking for `fixtures/bench/`.
/// Falls back to CWD if not found.
fn resolve_repo_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let cwd = std::env::current_dir().context("getting current dir")?;
    let mut cursor: &std::path::Path = &cwd;
    loop {
        if cursor.join("fixtures").join("bench").is_dir() {
            return Ok(cursor.to_path_buf());
        }
        match cursor.parent() {
            Some(p) => cursor = p,
            None => return Ok(cwd),
        }
    }
}

fn print_human_summary(report: &wash::bench::SuiteReport) {
    eprintln!("bench: label={} schema={}", report.label, report.schema_version);
    for task in &report.tasks {
        let status = if task.passed { "ok" } else { "FAIL" };
        eprintln!(
            "  [{status}] {:30}  calls={:>2}  bytes={:>7}  tokens={:>6}  cap={}  repeat={}",
            task.name,
            task.totals.call_count,
            task.totals.total_bytes,
            task.totals.total_tokens,
            task.totals.any_cap_hit,
            task.totals.any_repeated_call,
        );
        for e in &task.expectations {
            let ok = if e.passed { "ok" } else { "FAIL" };
            eprintln!("      - [{ok}] {}{}", e.name, fmt_detail(&e.detail));
        }
    }
    eprintln!(
        "  TOTAL: calls={} bytes={} tokens={} cap={} repeat={}",
        report.totals.call_count,
        report.totals.total_bytes,
        report.totals.total_tokens,
        report.totals.any_cap_hit,
        report.totals.any_repeated_call,
    );
}

fn fmt_detail(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        format!(" — {s}")
    }
}

fn print_compare_summary(
    before: &wash::bench::SuiteReport,
    after: &wash::bench::SuiteReport,
    outcome: &wash::bench::CompareOutcome,
) {
    eprintln!("compare: before={} after={}", before.label, after.label);
    eprintln!(
        "  suite: bytes {} -> {} (Δ {:+}, {:+.1}%), calls {} -> {} (Δ {:+}, {:+.1}%)",
        outcome.suite_delta.before.total_bytes,
        outcome.suite_delta.after.total_bytes,
        outcome.suite_delta.delta_bytes,
        outcome.suite_delta.bytes_growth * 100.0,
        outcome.suite_delta.before.call_count,
        outcome.suite_delta.after.call_count,
        outcome.suite_delta.delta_calls,
        outcome.suite_delta.calls_growth * 100.0,
    );
    for d in &outcome.task_deltas {
        eprintln!(
            "  {:30}  bytes Δ {:+} ({:+.1}%)  calls Δ {:+}  status={:?}",
            d.name,
            d.delta_bytes,
            d.bytes_growth * 100.0,
            d.delta_calls,
            d.status,
        );
        if !d.newly_failing_expectations.is_empty() {
            eprintln!(
                "    newly failing: {:?}",
                d.newly_failing_expectations
            );
        }
    }
    if outcome.regressions.is_empty() {
        eprintln!("  no regressions");
    } else {
        eprintln!("  regressions ({}):", outcome.regressions.len());
        for r in &outcome.regressions {
            eprintln!("    - {r}");
        }
    }
}
