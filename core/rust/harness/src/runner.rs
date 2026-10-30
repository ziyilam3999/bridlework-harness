//! Gate runner core (std only). Runs one declared check as a child process and returns a normalized `GateResult`.
//! A check past its time budget is FAIL (class timeout), never PASS. Captured output is capped in bytes and marked as cut.
//! A check path is resolved with realpath when loaded; a path that leaves the repo (dot-dot, absolute, or a symlink out) is refused.

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
    pub budget: Duration,
    pub max_output: usize,
}

impl Check {
    /// Resolve `program` (a path relative to the repo root) with realpath and refuse anything that lands outside the repo.
    pub fn load(repo_root: &Path, id: &str, program: &str, args: &[&str], budget: Duration, max_output: usize) -> Result<Check, String> {
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
        Ok(Check { id: id.to_string(), program: real, args: args.iter().map(|s| s.to_string()).collect(), budget, max_output })
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
    let mut child = match Command::new(&check.program)
        .args(&check.args)
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return result(check, head_sha, Status::Fail, Some("spawn-error"), "check could not be started".to_string()),
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
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if start.elapsed() >= check.budget {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(10));
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
        return result(check, head_sha, Status::Fail, Some("timeout"), message);
    }
    match status {
        Some(s) if s.success() => result(check, head_sha, Status::Pass, None, message),
        Some(_) => result(check, head_sha, Status::Fail, Some("check-failed"), message),
        None => result(check, head_sha, Status::Fail, Some("run-error"), message),
    }
}
