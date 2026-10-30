//! Gate runner core (std only). Runs one declared check as a child process and returns a normalized `GateResult`.
//! A check past its time budget is FAIL (class timeout), never PASS. Captured output is capped in bytes and marked as cut.
//! A check path is resolved with realpath when loaded; a path that leaves the repo (dot-dot, absolute, or a symlink out) is refused.
//! Known limits: the resolved path is not re-checked at run time (load-to-run swap is not closed), and only the direct child is killed, not its descendants.

use crate::gate_result::{GateResult, Status};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// A check that passed `load`: its program is a real path inside the repo.
#[derive(Debug, Clone)]
pub struct Check {
    pub id: String,
    program: PathBuf,
    pub args: Vec<String>,
    budget: Duration,
    max_output: usize,
    poll: Duration,
}

impl Check {
    /// Resolve `program` (a path relative to the repo root) with realpath and refuse anything that lands outside the repo.
    pub fn load(repo_root: &Path, id: &str, program: &str, args: &[&str], budget: Duration, max_output: usize) -> Result<Check, String> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')) {
            return Err("check id malformed".to_string());
        }
        if program.is_empty() || Path::new(program).is_absolute() {
            return Err("check path must be relative to the repo".to_string());
        }
        let root = repo_root.canonicalize().map_err(|_| "repo root unreadable".to_string())?;
        let real = root.join(program).canonicalize().map_err(|_| "check path does not resolve".to_string())?;
        if !real.starts_with(&root) {
            return Err("check path escapes the repo".to_string());
        }
        if !real.is_file() {
            return Err("check path is not a file".to_string());
        }
        if max_output == 0 || budget.is_zero() {
            return Err("budget and output cap must be above zero".to_string());
        }
        Ok(Check { id: id.to_string(), program: real, args: args.iter().map(|s| s.to_string()).collect(), budget, max_output, poll: Duration::from_millis(10) })
    }
}

/// Keep at most `cap` bytes in `buf`; drain the rest so the child never blocks on a full pipe. Returns whether bytes were cut.
fn capture<R: Read>(mut r: R, buf: Arc<Mutex<Vec<u8>>>, cap: usize, cut: Arc<Mutex<bool>>, done: mpsc::Sender<()>) {
    let mut chunk = [0u8; 4096];
    loop {
        match r.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut b = buf.lock().unwrap();
                let room = cap.saturating_sub(b.len());
                if n > room {
                    *cut.lock().unwrap() = true;
                }
                b.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    let _ = done.send(());
}

fn result(check: &Check, head_sha: &str, status: Status, class: Option<&str>, message: String) -> GateResult {
    GateResult { status, gate_id: check.id.clone(), head_sha: head_sha.to_string(), failure_class: class.map(|c| c.to_string()), message, file: None, line: None }
}

/// Run the check in `repo_root`. Never panics on a check's behavior; every outcome is a normalized result.
pub fn run(repo_root: &Path, check: &Check, head_sha: &str) -> GateResult {
    run_code(repo_root, check, head_sha).0
}

/// Like `run`, also returning the check's own exit code when it exited normally (None for a timeout, a signal or a spawn failure).
pub fn run_code(repo_root: &Path, check: &Check, head_sha: &str) -> (GateResult, Option<i32>) {
    // The budget clock starts before spawn, so spawn and reader setup count against it.
    let start = Instant::now();
    let mut cmd = Command::new(&check.program);
    cmd.args(&check.args).current_dir(repo_root).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // A script written just before the run can briefly be "text file busy" while another thread forks; retry a few times, then fail.
    let mut tries = 0;
    let mut child = loop {
        match cmd.spawn() {
            Ok(c) => break c,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 10 => {
                tries += 1;
                thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let m = format!("check could not be started ({:?})", e.kind());
                return (result(check, head_sha, Status::Fail, Some("spawn-error"), m), None);
            }
        }
    };
    let buf = Arc::new(Mutex::new(Vec::new()));
    let cut = Arc::new(Mutex::new(false));
    let (tx, rx) = mpsc::channel();
    // One shared cap across stdout and stderr; readers are detached so a grandchild holding a pipe cannot hang the runner.
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten() {
        let (b, c, t, cap) = (buf.clone(), cut.clone(), tx.clone(), check.max_output);
        thread::spawn(move || capture(stream, b, cap, c, t));
    }
    drop(tx);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            // An exit seen only after the budget has run out is still a timeout: fail closed.
            Ok(Some(s)) => {
                if start.elapsed() >= check.budget {
                    timed_out = true;
                }
                break Some(s);
            }
            Ok(None) => {
                if start.elapsed() >= check.budget {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(check.poll);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // Give the readers a short grace to finish after the child is gone, then take what was captured.
    let grace = Instant::now();
    let mut finished = 0;
    while finished < 2 && grace.elapsed() < Duration::from_millis(300) {
        if rx.recv_timeout(Duration::from_millis(20)).is_ok() {
            finished += 1;
        } else if matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
            break;
        }
    }
    let mut message = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    if *cut.lock().unwrap() {
        message.push_str(&format!("\n[output cut at {} bytes]", check.max_output));
    }
    if timed_out {
        return (result(check, head_sha, Status::Fail, Some("timeout"), message), None);
    }
    match status {
        Some(s) if s.success() => (result(check, head_sha, Status::Pass, None, message), Some(0)),
        Some(s) => (result(check, head_sha, Status::Fail, Some("check-failed"), message), s.code()),
        None => (result(check, head_sha, Status::Fail, Some("run-error"), message), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long poll gap makes the late-exit race deterministic: the child exits 0 well inside the gap, and the first look at it is past the budget.
    #[cfg(unix)]
    #[test]
    fn exit_seen_only_after_the_budget_is_a_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("hr-late-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("quick.sh");
        std::fs::write(&f, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut c = Check::load(&d, "late", "quick.sh", &[], Duration::from_millis(100), 100).unwrap();
        c.poll = Duration::from_millis(400);
        let r = run(&d, &c, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("timeout")));
        let _ = std::fs::remove_dir_all(d);
    }
}
