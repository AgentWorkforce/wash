//! Shared subprocess plumbing for the process-backed tools (Build, TestRun,
//! GitState, GhPR).
//!
//! Each of those tools used to hand-roll the same spawn → capture → lossy-decode
//! → price sequence with small accidental differences (most visibly, two
//! different `baselineBytes` formulas). Centralizing it here gives one definition
//! of how wash runs a child process and one definition of what a subprocess tool
//! "would have cost" via vanilla Bash.
//!
//! Every spawn is bounded by a caller-supplied deadline. The MCP server handles
//! `tools/call` synchronously on its read loop, so an unbounded child (a build
//! waiting on stdin, a `gh` call stuck on a network prompt, a hung test) would
//! stall the entire session with no way to recover. [`run`] kills the child once
//! the deadline passes and reports it via [`Captured::timed_out`], preserving
//! whatever partial output was captured before the kill.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How often the wait loop polls the child while waiting for it to exit or for
/// the deadline to pass. Small enough that fast commands add negligible latency,
/// large enough not to busy-spin.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long to wait for the pipe readers to drain after the child has exited or
/// been killed. On a clean exit the readers hit EOF almost instantly; this bound
/// only matters when a *grandchild* the command spawned inherited the pipe and is
/// still holding it open — `child.kill()` reaps only the direct child, so without
/// a bound the readers would never see EOF and we would hang forever, defeating
/// the whole point of the timeout. After the grace we return the partial output
/// captured so far and let the now-detached readers exit when the pipe closes.
const READER_GRACE: Duration = Duration::from_secs(2);

/// A finished subprocess: both streams lossily decoded to `String`, the exit
/// code (if any), and the raw byte count used to price vanilla output.
pub struct Captured {
    pub stdout: String,
    pub stderr: String,
    /// Exit code, or `None` if the process was killed by a signal (including a
    /// timeout kill — see [`timed_out`](Self::timed_out)).
    pub status: Option<i32>,
    /// Raw stdout+stderr byte count — see [`subprocess_baseline`].
    pub baseline: u64,
    /// `true` if the process was killed for exceeding its deadline. The captured
    /// `stdout`/`stderr` then hold whatever was produced before the kill.
    pub timed_out: bool,
}

impl Captured {
    fn from_parts(stdout: Vec<u8>, stderr: Vec<u8>, status: Option<i32>, timed_out: bool) -> Self {
        Self {
            baseline: subprocess_baseline(&stdout, &stderr),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            status,
            timed_out,
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

/// Spawn `program args` in `cwd` with stdin closed, capturing both streams, and
/// kill the child if it runs longer than `timeout`.
///
/// Returns `Err` only when the process fails to *spawn* (e.g. binary missing); a
/// non-zero exit is a successful capture — inspect [`Captured::status`]. A
/// timeout is likewise a successful capture with [`Captured::timed_out`] set and
/// partial output preserved; callers decide how to surface it.
pub fn run<P, I, S>(
    program: P,
    args: I,
    cwd: impl AsRef<Path>,
    timeout: Duration,
) -> std::io::Result<Captured>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Drain each pipe on its own thread into a shared buffer. A child that writes
    // more than the OS pipe buffer (~64KB) blocks on write until someone reads;
    // if we only read after `wait`, that's a deadlock. Draining concurrently also
    // means partial output survives a timeout kill. The buffers are shared (not
    // returned via the thread) so we can read whatever was captured *without*
    // joining — see the bounded grace wait below.
    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    drain(child.stdout.take().expect("stdout piped"), Arc::clone(&out_buf), done_tx.clone());
    drain(child.stderr.take().expect("stderr piped"), Arc::clone(&err_buf), done_tx);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            // try_wait failing is almost impossible for a child we own, but if it
            // does, kill and stop waiting rather than `?`-returning — that would
            // leave the child unreaped (a zombie; `Child::drop` neither waits nor
            // kills) and abandon the readers mid-drain.
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
        }
    };

    // Wait for the readers to finish, but only up to READER_GRACE: a grandchild
    // that inherited the pipe can keep it open past the direct child's death, and
    // an unbounded join there would hang the server. On the common path both
    // readers signal within microseconds of the child closing its pipes.
    let grace_until = Instant::now() + READER_GRACE;
    for _ in 0..2 {
        let wait = grace_until.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(wait).is_err() {
            break;
        }
    }

    // Take whatever has been captured so far. If a reader is still blocked on an
    // orphaned grandchild it keeps its own Arc clone and exits when the pipe
    // finally closes; we've already moved the data out, so nothing is lost and
    // nothing stalls.
    let stdout = take_buf(&out_buf);
    let stderr = take_buf(&err_buf);

    Ok(Captured::from_parts(stdout, stderr, status, timed_out))
}

/// Spawn a thread that drains `pipe` into `buf` until EOF, then signals on `done`.
/// Detached by design (the handle is dropped): the bounded grace wait in [`run`]
/// reads `buf` directly, so a reader stuck on an orphaned grandchild never blocks
/// the caller.
fn drain<R: Read + Send + 'static>(mut pipe: R, buf: Arc<Mutex<Vec<u8>>>, done: mpsc::Sender<()>) {
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut g) = buf.lock() {
                        g.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        let _ = done.send(());
    });
}

/// Move the captured bytes out of a shared buffer, tolerating a poisoned lock
/// (a reader panicking mid-drain shouldn't crash the tool — just yield what we have).
fn take_buf(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    match buf.lock() {
        Ok(mut g) => std::mem::take(&mut *g),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A timeout generous enough that the "fast command" cases never trip it.
    const GENEROUS: Duration = Duration::from_secs(30);

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
        let c = run("cmd", ["/C", "echo hi"], &dir, GENEROUS).expect("cmd spawns");
        #[cfg(not(windows))]
        let c = run("echo", ["hi"], &dir, GENEROUS).expect("echo spawns");
        assert_eq!(c.status, Some(0));
        assert!(!c.timed_out);
        assert_eq!(c.stdout.trim_end(), "hi");
        assert!(c.stderr.is_empty());
        // No invalid UTF-8, so raw byte count equals the decoded string length.
        assert_eq!(c.baseline, c.stdout.len() as u64);
    }

    #[test]
    fn run_errors_when_program_missing() {
        let dir = std::env::temp_dir();
        assert!(run("wash-no-such-binary-xyz", ["x"], &dir, GENEROUS).is_err());
    }

    #[test]
    fn completes_fast_command() {
        // git is present wherever this crate's tests run (the repo is a git repo
        // and CI installs it). A 30s budget is never hit by `git --version`.
        let dir = std::env::temp_dir();
        let c = run("git", ["--version"], &dir, GENEROUS).expect("git spawns");
        assert!(!c.timed_out);
        assert_eq!(c.status, Some(0));
        assert!(!c.stdout.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn times_out_and_kills() {
        let dir = std::env::temp_dir();
        let t0 = Instant::now();
        let c = run("sh", ["-c", "sleep 30"], &dir, Duration::from_millis(200)).expect("sh spawns");
        assert!(c.timed_out, "expected the 30s sleep to be killed");
        // The kill must happen near the deadline, nowhere near the 30s sleep.
        assert!(t0.elapsed() < Duration::from_secs(5), "took {:?}", t0.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn does_not_hang_when_grandchild_keeps_pipe_open() {
        // The shell exits 0 immediately but backgrounds a `sleep` that inherits
        // the stdout pipe and holds it open for 30s. A plain `read_to_end` +
        // `join` would block ~30s waiting for that grandchild's EOF; the bounded
        // reader grace must let `run` return promptly with the output captured so
        // far. This is the regression test for the orphaned-grandchild hang.
        let dir = std::env::temp_dir();
        let t0 = Instant::now();
        let c = run("sh", ["-c", "echo started; sleep 30 &"], &dir, GENEROUS).expect("sh spawns");
        assert_eq!(c.status, Some(0));
        assert!(!c.timed_out, "the shell exited cleanly; this is not a timeout");
        assert!(c.stdout.contains("started"), "stdout was {:?}", c.stdout);
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "run() blocked on the grandchild for {:?}",
            t0.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn captures_partial_output_on_timeout() {
        let dir = std::env::temp_dir();
        let c = run(
            "sh",
            ["-c", "echo early; sleep 30"],
            &dir,
            Duration::from_millis(300),
        )
        .expect("sh spawns");
        assert!(c.timed_out);
        assert!(c.stdout.contains("early"), "partial stdout was {:?}", c.stdout);
    }
}
