//! Shared subprocess plumbing for the process-backed tools (Build, TestRun,
//! GitState, GhPR).
//!
//! Each of those tools used to hand-roll the same spawn → capture → lossy-decode
//! → price sequence with small accidental differences (most visibly, two
//! different `baselineBytes` formulas). Centralizing it here gives one definition
//! of how wash runs a child process and one definition of what a subprocess tool
//! "would have cost" via vanilla Bash.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// A finished subprocess: both streams lossily decoded to `String`, the exit
/// code (if any), and the raw byte count used to price vanilla output.
pub struct Captured {
    pub stdout: String,
    pub stderr: String,
    /// Exit code, or `None` if the process was killed by a signal.
    pub status: Option<i32>,
    /// Raw stdout+stderr byte count — see [`subprocess_baseline`].
    pub baseline: u64,
}

impl Captured {
    fn from_output(out: Output) -> Self {
        Self {
            baseline: subprocess_baseline(&out.stdout, &out.stderr),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            status: out.status.code(),
        }
    }
}

/// The raw bytes an agent would have paid for had it run this command through
/// vanilla Bash: both streams, as captured, *before* lossy UTF-8 decoding so the
/// estimate can't drift on invalid bytes.
///
/// This is the single definition of `baselineBytes` for subprocess tools. Read
/// prices full file bytes instead (see `meta.rs`). Where one tool call makes
/// several subprocess calls, the baseline is the sum of all of them.
pub fn subprocess_baseline(stdout: &[u8], stderr: &[u8]) -> u64 {
    (stdout.len() + stderr.len()) as u64
}

/// Spawn `program args` in `cwd` with stdin closed, capturing both streams.
///
/// Returns `Err` only when the process fails to *spawn* (e.g. binary missing);
/// a non-zero exit is a successful capture — inspect [`Captured::status`].
pub fn run<P, I, S>(program: P, args: I, cwd: impl AsRef<Path>) -> std::io::Result<Captured>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map(Captured::from_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_sums_raw_bytes_before_decode() {
        // Invalid UTF-8 in stdout: baseline counts raw bytes, not decoded chars.
        assert_eq!(subprocess_baseline(&[0xff, 0xfe], b"err"), 5);
    }

    #[test]
    fn run_captures_stdout_and_zero_exit() {
        let dir = std::env::temp_dir();
        // `echo` is a cmd.exe builtin on Windows (no echo.exe to spawn), so pick a
        // portable invocation per platform — the capture/baseline logic is identical.
        #[cfg(windows)]
        let c = run("cmd", ["/C", "echo hi"], &dir).expect("cmd spawns");
        #[cfg(not(windows))]
        let c = run("echo", ["hi"], &dir).expect("echo spawns");
        assert_eq!(c.status, Some(0));
        assert_eq!(c.stdout.trim_end(), "hi");
        assert!(c.stderr.is_empty());
        // No invalid UTF-8, so raw byte count equals the decoded string length.
        assert_eq!(c.baseline, c.stdout.len() as u64);
    }

    #[test]
    fn run_errors_when_program_missing() {
        let dir = std::env::temp_dir();
        assert!(run("wash-no-such-binary-xyz", ["x"], &dir).is_err());
    }
}
