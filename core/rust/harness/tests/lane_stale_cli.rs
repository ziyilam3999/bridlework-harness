//! `harness lane stale`: the report of which live lanes belong to a gone session with nothing unsaved. Real binary, a stub host script,
//! synthetic launch records and lane records, and throwaway git repos with a local bare remote (no network). A run has a time bound, and
//! a test checks that the lanes dir is byte-unchanged after the run.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// A launch time far after any lane file time, so a lane file does not read as claimed after its launch.
const FAR: u64 = 4_102_444_800_000;

/// A child is spawned while this lock is held, and so is the write of a stub script: no fork can then hold the script's write handle when
/// the script is run (the text-file-busy error of a script run right after it was written).
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn spawn(c: &mut Command) -> std::process::Child {
    let _g = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    c.spawn().unwrap()
}

/// The one stub host script of this process, written once. Each fixture links to it (`--host` may be a link) and sets what it does by
/// its own `mode` and `list.json` files next to the link: a fresh executable is checked by the OS on its first run and the checks
/// queue, so one script per test would pass the 10 second listing limit on a busy machine.
fn shared_host() -> &'static Path {
    static HOST: OnceLock<PathBuf> = OnceLock::new();
    HOST.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("lstale-host-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.canonicalize().unwrap().join("host.sh");
        let body = format!(
            "#!/bin/sh\n\
             d=$(dirname \"$0\")\n\
             case $(cat \"$d/mode\" 2>/dev/null) in\n\
             fail) exit 3 ;;\n\
             hang) sleep 60 ;;\n\
             junk) echo nope ;;\n\
             lock)\n\
             '{BIN}' lane list --lanes-dir \"$d/lanes\" --config \"$d/config.yaml\" > /dev/null\n\
             echo \"list-exit=$?\" >> \"$d/calls\"\n\
             cat \"$d/list.json\" ;;\n\
             *)\n\
             echo \"$@\" >> \"$d/calls\"\n\
             cat \"$d/list.json\" ;;\n\
             esac\n"
        );
        let _g = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    })
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = spawn(c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())).wait_with_output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run_bounded(args: &[String], secs: u64) -> Output {
    let child = spawn(Command::new(BIN).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()));
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(o) => o.unwrap(),
        Err(_) => {
            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
            panic!("the run did not end within {secs} seconds");
        }
    }
}

fn exec_name(lane: u32, slug: &str) -> String {
    ["exec".to_string(), format!("L{lane}"), format!("t{slug}"), "r1".into(), "M".into(), "son".into(), "med".into()].join("-")
}

fn ent(id: &str, name: &str, state: Option<&str>) -> String {
    match state {
        Some(s) => format!("{{\"id\":\"{id}\",\"name\":\"{name}\",\"state\":\"{s}\"}}"),
        None => format!("{{\"id\":\"{id}\",\"name\":\"{name}\"}}"),
    }
}

fn line(ticket: &str, lane: u32, verdict: &str, reason: &str) -> String {
    format!("stale ticket={ticket} lane={lane} verdict={verdict} reason={reason} action=none")
}

fn tail(checked: u32, unknown: u32) -> String {
    format!("stale checked={checked} parked=0 unknown={unknown}")
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> = match fs::read_dir(dir) {
        Ok(rd) => rd.map(|e| e.unwrap()).map(|e| (e.file_name().to_string_lossy().to_string(), fs::read(e.path()).unwrap_or_default())).collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

struct Fx {
    root: PathBuf,
    lanes: PathBuf,
    md: PathBuf,
    wroot: PathBuf,
    records: PathBuf,
    host: PathBuf,
    list: PathBuf,
    calls: PathBuf,
    cfg: PathBuf,
    repo: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let root = std::env::temp_dir().join(format!("lstale-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let fx = Fx {
            lanes: root.join("lanes"),
            md: root.join("M"),
            wroot: root.join("W"),
            records: root.join("launches.jsonl"),
            host: root.join("host.sh"),
            list: root.join("list.json"),
            calls: root.join("calls"),
            cfg: root.join("config.yaml"),
            repo: root.join("R"),
            root,
        };
        for d in [&fx.lanes, &fx.md, &fx.wroot, &fx.repo] {
            fs::create_dir_all(d).unwrap();
        }
        fs::copy(EXAMPLE, &fx.cfg).unwrap();
        fs::write(&fx.records, "").unwrap();
        fs::write(&fx.calls, "").unwrap();
        fx.entries(&[]);
        symlink(shared_host(), &fx.host).unwrap();
        fx
    }

    /// A fixture with lane 1 held by ticket `stale` and one bound exec launch for it.
    fn basic() -> Fx {
        let fx = Fx::new();
        fx.lane(1, "stale", "r");
        fx.bound(1, "stale", FAR);
        fx
    }

    /// What the shared host script does for this fixture: `fail`, `hang`, `junk` or `lock` (the default prints `list.json`).
    fn mode(&self, m: &str) {
        fs::write(self.root.join("mode"), m).unwrap();
    }

    fn entries(&self, e: &[String]) {
        fs::write(&self.list, format!("[{}]\n", e.join(","))).unwrap();
    }

    fn lane(&self, n: u32, ticket: &str, repo: &str) {
        fs::create_dir_all(&self.lanes).unwrap();
        fs::write(self.lanes.join(format!("lane-{n}")), format!("lane-record v1\nticket={ticket}\nrepo_id={repo}\nscope=a\n")).unwrap();
    }

    fn park(&self, ticket: &str) {
        fs::write(self.lanes.join(format!("parked-{ticket}")), format!("lane-record v1\nticket={ticket}\nrepo_id=r\nscope=a\n")).unwrap();
    }

    fn append(&self, text: &str) {
        let mut s = fs::read_to_string(&self.records).unwrap();
        s.push_str(text);
        fs::write(&self.records, s).unwrap();
    }

    /// One launch row by state: `bound` (launch and ids rows), `pending` (launch row only), `failed` (launch and failure rows).
    fn launch(&self, name: &str, ts: u64, state: &str) {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let run = format!("run{n}");
        let sha = "a".repeat(64);
        self.append(&format!("{{\"v\":1,\"kind\":\"launch\",\"ts_ms\":{ts},\"run_id\":\"{run}\",\"name\":\"{name}\",\"role\":\"exec\",\"brief_sha256\":\"{sha}\",\"owner\":\"orchestration\"}}\n"));
        match state {
            "bound" => {
                let uuid = format!("{:08x}-abcd-4123-8456-{:012x}", 0x1234_5678u32 + n as u32, n);
                self.append(&format!("{{\"v\":1,\"kind\":\"launch_ids\",\"ts_ms\":{ts},\"run_id\":\"{run}\",\"short_id\":\"s{n}\",\"session_id\":\"{uuid}\"}}\n"));
            }
            "failed" => self.append(&format!("{{\"v\":1,\"kind\":\"launch_failed\",\"ts_ms\":{ts},\"run_id\":\"{run}\",\"stage\":\"start\",\"reason\":\"x\"}}\n")),
            _ => {}
        }
    }

    fn bound(&self, lane: u32, slug: &str, ts: u64) {
        self.launch(&exec_name(lane, slug), ts, "bound");
    }

    /// The ticket worktree made by the real `workspace create`, on a branch pushed to a local bare remote when `push` is set.
    fn ws(&self, ticket: &str, push: bool) -> PathBuf {
        if !self.repo.join(".git").exists() {
            fs::create_dir_all(&self.repo).unwrap();
            git(&self.repo, &["init", "-q"]);
            for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
                git(&self.repo, &["config", k, v]);
            }
            fs::write(self.repo.join("f.txt"), "one").unwrap();
            git(&self.repo, &["add", "f.txt"]);
            git(&self.repo, &["commit", "-q", "-m", "one"]);
            let remote = self.root.join("origin.git");
            fs::create_dir_all(&remote).unwrap();
            git(&remote, &["init", "-q", "--bare"]);
            for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
                git(&remote, &["config", k, v]);
            }
            git(&self.repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        }
        let base = git(&self.repo, &["rev-parse", "HEAD"]);
        let tf = self.root.join(format!("{ticket}.json"));
        fs::write(&tf, format!("{{\"id\":\"{ticket}\",\"repo_id\":\"r\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"M\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}")).unwrap();
        let args: Vec<String> = [
            "workspace",
            "create",
            tf.to_str().unwrap(),
            "--repo-map",
            &format!("r={}", self.repo.display()),
            "--base",
            &base,
            "--workspace-root",
            self.wroot.to_str().unwrap(),
            "--manifest-dir",
            self.md.to_str().unwrap(),
        ]
        .iter()
        .map(|x| x.to_string())
        .collect();
        let o = run_bounded(&args, 60);
        assert_eq!(o.status.code(), Some(0), "create: {}", String::from_utf8_lossy(&o.stderr));
        let tree = self.wroot.join(ticket).join("tree");
        if push {
            git(&tree, &["push", "-q", "-u", "origin", &format!("ws/{ticket}")]);
        }
        tree
    }

    fn args(&self) -> Vec<String> {
        [
            "lane".to_string(),
            "stale".into(),
            "--lanes-dir".into(),
            self.lanes.to_str().unwrap().into(),
            "--config".into(),
            self.cfg.to_str().unwrap().into(),
            "--host".into(),
            self.host.to_str().unwrap().into(),
            "--launch-records".into(),
            self.records.to_str().unwrap().into(),
            "--manifest-dir".into(),
            self.md.to_str().unwrap().into(),
            "--repo-map".into(),
            format!("r={}", self.repo.display()),
        ]
        .to_vec()
    }

    /// Run with `extra` options added; check the exit code, the exact stdout lines and the unchanged lanes dir.
    fn expect(&self, extra: &[&str], code: i32, want: &[String]) {
        let mut a = self.args();
        a.extend(extra.iter().map(|s| s.to_string()));
        let before = snapshot(&self.lanes);
        let o = run_bounded(&a, 120);
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        let got: Vec<&str> = out.lines().collect();
        assert_eq!(got, want.iter().map(|s| s.as_str()).collect::<Vec<_>>(), "stdout (stderr: {})", String::from_utf8_lossy(&o.stderr));
        assert_eq!(o.status.code(), Some(code), "exit code (stderr: {})", String::from_utf8_lossy(&o.stderr));
        assert_eq!(snapshot(&self.lanes), before, "the lanes dir changed");
    }

    /// A whole-run failure: exit 2, empty stdout, one `lane: error:` line that holds `cause`, the lanes dir unchanged.
    fn refuse(&self, a: &[String], cause: &str, secs: u64) {
        let before = snapshot(&self.lanes);
        let o = run_bounded(a, secs);
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        assert_eq!(o.status.code(), Some(2), "exit code (stderr: {err})");
        assert!(o.stdout.is_empty(), "stdout must be empty: {}", String::from_utf8_lossy(&o.stdout));
        assert_eq!(err.lines().count(), 1, "one stderr line: {err}");
        assert!(err.starts_with("lane: error: "), "{err}");
        assert!(err.contains(cause), "the line must name `{cause}`: {err}");
        assert_eq!(snapshot(&self.lanes), before, "the lanes dir changed");
    }

    fn with(&self, opt: &str, val: &str) -> Vec<String> {
        let mut a = self.args();
        let i = a.iter().position(|x| x == opt).unwrap();
        a[i + 1] = val.to_string();
        a
    }

    fn fifo(&self, p: &Path) {
        let st = spawn(Command::new("mkfifo").arg(p)).wait().unwrap();
        assert!(st.success());
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stale_one() -> Vec<String> {
    vec![line("stale", 1, "stale", "gone-clean-pushed"), tail(1, 0)]
}

// ---------------------------------------------------------------------------------------------- verdicts

#[test]
fn gone_clean_pushed_is_stale() {
    let fx = Fx::basic();
    fx.ws("stale", true);
    fx.expect(&[], 0, &stale_one());
}

#[test]
fn not_listed_at_all_is_gone() {
    let fx = Fx::basic();
    fx.ws("stale", true);
    fx.entries(&[ent("o1", &exec_name(2, "other"), Some("working"))]);
    fx.expect(&[], 0, &stale_one());
}

#[test]
fn only_done_or_stopped_entries_is_gone() {
    let fx = Fx::basic();
    fx.ws("stale", true);
    fx.entries(&[ent("a1", &exec_name(1, "stale"), Some("done")), ent("a2", &exec_name(1, "stale"), Some("stopped"))]);
    fx.expect(&[], 0, &stale_one());
}

#[test]
fn a_running_session_is_alive() {
    // `working` with no pid and no status key is the shape the probe saw right after a kill -9
    for st in [Some("working"), Some("blocked"), Some("sleeping"), None] {
        let fx = Fx::basic();
        fx.entries(&[ent("a1", &exec_name(1, "stale"), st)]);
        fx.expect(&[], 0, &[line("stale", 1, "alive", "listed-running"), tail(1, 0)]);
    }
}

#[test]
fn two_entries_one_running_is_alive() {
    for (s1, s2) in [("done", "working"), ("working", "stopped")] {
        let fx = Fx::basic();
        fx.entries(&[ent("a1", &exec_name(1, "stale"), Some(s1)), ent("a2", &exec_name(1, "stale"), Some(s2))]);
        fx.expect(&[], 0, &[line("stale", 1, "alive", "listed-running"), tail(1, 0)]);
    }
}

#[test]
fn reviewer_entry_running_does_not_hold_the_lane() {
    let fx = Fx::basic();
    fx.ws("stale", true);
    fx.entries(&[ent("r1", "e-rvw-L1-tstale-r1-M-opu-hig", Some("working")), ent("a1", &exec_name(1, "stale"), Some("stopped"))]);
    fx.expect(&[], 0, &stale_one());
}

#[test]
fn no_launch_row_is_hand_and_the_tree_is_not_read() {
    let fx = Fx::new();
    fx.lane(1, "stale", "r");
    fx.expect(&[], 0, &[line("stale", 1, "hand", "no-exec-launch"), tail(1, 0)]);
}

#[test]
fn failed_launch_row_is_not_a_session() {
    let fx = Fx::new();
    fx.lane(1, "stale", "r");
    fx.launch(&exec_name(1, "stale"), FAR, "failed");
    fx.append(&format!("{{\"v\":1,\"kind\":\"launch_refused\",\"ts_ms\":{FAR},\"run_id\":\"refused1\",\"name\":\"{}\",\"role\":\"exec\",\"brief_sha256\":\"{}\",\"matched_entry\":\"x\"}}\n", exec_name(1, "stale"), "a".repeat(64)));
    fx.expect(&[], 0, &[line("stale", 1, "hand", "no-exec-launch"), tail(1, 0)]);
}

#[test]
fn pending_launch_row_is_alive() {
    let fx = Fx::new();
    fx.lane(1, "stale", "r");
    fx.launch(&exec_name(1, "stale"), FAR, "pending");
    fx.expect(&[], 0, &[line("stale", 1, "alive", "launch-pending"), tail(1, 0)]);
}

#[test]
fn lane_claimed_after_last_launch_is_alive() {
    let fx = Fx::new();
    fx.lane(1, "stale", "r");
    fx.bound(1, "stale", 1000);
    fx.expect(&[], 0, &[line("stale", 1, "alive", "claimed-after-launch"), tail(1, 0)]);
    // a launch after the lane file time is judged on
    let fx = Fx::basic();
    fx.ws("stale", true);
    fx.expect(&[], 0, &stale_one());
}

#[test]
fn listed_lane_above_config_count_still_counts() {
    let fx = Fx::basic();
    fx.entries(&[ent("a1", &exec_name(9, "stale"), Some("working"))]);
    fx.expect(&[], 0, &[line("stale", 1, "alive", "listed-running"), tail(1, 0)]);
}

#[test]
fn launch_row_above_config_count_still_counts() {
    let fx = Fx::new();
    fx.lane(1, "stale", "r");
    fx.bound(9, "stale", FAR);
    fx.ws("stale", true);
    fx.expect(&[], 0, &stale_one());
}

#[test]
fn prefix_twin_name_does_not_count() {
    let fx = Fx::basic();
    fx.ws("stale", true);
    fx.entries(&[ent("a1", &exec_name(1, "stalelane"), Some("working"))]);
    fx.expect(&[], 0, &stale_one());
}

#[test]
fn letter_case_name_is_name_unclear() {
    let fx = Fx::basic();
    fx.entries(&[ent("a1", "exec-L1-tSTALE-r1-M-son-med", Some("working"))]);
    fx.expect(&[], 3, &[line("stale", 1, "unknown", "name-unclear"), tail(1, 1)]);
}

#[test]
fn hyphen_read_back_is_name_unclear() {
    let fx = Fx::new();
    fx.lane(1, "stalelane", "r");
    fx.bound(1, "stalelane", FAR);
    fx.entries(&[ent("a1", "execution-l1-stale-lane", Some("working"))]);
    fx.expect(&[], 3, &[line("stalelane", 1, "unknown", "name-unclear"), tail(1, 1)]);
}

#[test]
fn slug_shared_by_two_live_tickets_is_unknown() {
    let fx = Fx::new();
    fx.lane(1, "a-b", "r");
    fx.lane(2, "ab", "r");
    fx.bound(1, "ab", FAR);
    fx.expect(&[], 3, &[line("a-b", 1, "unknown", "slug-shared"), line("ab", 2, "unknown", "slug-shared"), tail(2, 2)]);
}

#[test]
fn slug_shared_with_a_parked_ticket_is_unknown() {
    let fx = Fx::new();
    fx.lane(1, "a-b", "r");
    fx.park("ab");
    fx.bound(1, "ab", FAR);
    fx.expect(&[], 3, &[line("a-b", 1, "unknown", "slug-shared"), tail(1, 1)]);
}

#[test]
fn live_and_parked_ticket_is_unknown() {
    let fx = Fx::basic();
    fx.ws("stale", true);
    fx.park("stale");
    fx.expect(&[], 3, &[line("stale", 1, "unknown", "live-and-parked"), tail(1, 1)]);
}

#[test]
fn parked_records_are_not_judged() {
    let fx = Fx::new();
    fx.park("gone");
    fx.expect(&[], 0, &[tail(0, 0)]);
    fx.lane(1, "stale", "r");
    fx.bound(1, "stale", FAR);
    fx.ws("stale", true);
    fx.expect(&[], 0, &stale_one());
}

#[test]
fn a_ticket_id_with_no_letter_or_digit_is_hand() {
    // the id has an empty slug: `hand no-exec-launch` before the slug-shared and live-and-parked rules
    let fx = Fx::new();
    fx.lane(1, "_", "r");
    fx.park("_");
    fx.expect(&[], 0, &[line("_", 1, "hand", "no-exec-launch"), tail(1, 0)]);
}

// ---------------------------------------------------------------------------------------------- the tree

#[test]
fn dirty_tree_is_unsaved() {
    let fx = Fx::basic();
    let tree = fx.ws("stale", true);
    fs::write(tree.join("f.txt"), "changed").unwrap();
    fx.expect(&[], 0, &[line("stale", 1, "unsaved", "stray"), tail(1, 0)]);
}

#[test]
fn untracked_file_is_unsaved() {
    let fx = Fx::basic();
    let tree = fx.ws("stale", true);
    fs::write(tree.join("new.txt"), "x").unwrap();
    fx.expect(&[], 0, &[line("stale", 1, "unsaved", "stray"), tail(1, 0)]);
}

fn ignored_build_output(fx: &Fx, tree: &Path) {
    let info = fx.repo.join(".git").join("info");
    fs::create_dir_all(&info).unwrap();
    fs::write(info.join("exclude"), "core/rust/target/\n").unwrap();
    fs::create_dir_all(tree.join("core/rust/target")).unwrap();
    fs::write(tree.join("core/rust/target/x.o"), "x").unwrap();
}

#[test]
fn ignored_build_output_without_allow_is_unsaved() {
    let fx = Fx::basic();
    let tree = fx.ws("stale", true);
    ignored_build_output(&fx, &tree);
    fx.expect(&[], 0, &[line("stale", 1, "unsaved", "stray"), tail(1, 0)]);
}

#[test]
fn allowed_build_output_and_message_file_is_stale() {
    let fx = Fx::basic();
    let tree = fx.ws("stale", true);
    ignored_build_output(&fx, &tree);
    fs::write(tree.join(".bw-commit-msg"), "msg").unwrap();
    fx.expect(&["--allow", "core/rust/target", "--allow", ".bw-commit-msg"], 0, &stale_one());
}

#[test]
fn allow_does_not_cover_a_modified_path() {
    let fx = Fx::basic();
    let tree = fx.ws("stale", true);
    fs::write(tree.join("f.txt"), "changed").unwrap();
    fx.expect(&["--allow", "f.txt"], 0, &[line("stale", 1, "unsaved", "stray"), tail(1, 0)]);
}

#[test]
fn allow_does_not_cover_a_path_below_it() {
    let fx = Fx::basic();
    let tree = fx.ws("stale", true);
    fs::create_dir_all(tree.join("a")).unwrap();
    fs::write(tree.join("a/b"), "x").unwrap();
    fx.expect(&["--allow", "a"], 0, &[line("stale", 1, "unsaved", "stray"), tail(1, 0)]);
}

#[test]
fn unpushed_commit_is_unsaved() {
    let fx = Fx::basic();
    let tree = fx.ws("stale", true);
    fs::write(tree.join("g.txt"), "two").unwrap();
    git(&tree, &["add", "g.txt"]);
    git(&tree, &["commit", "-q", "-m", "two"]);
    fx.expect(&[], 0, &[line("stale", 1, "unsaved", "unpushed"), tail(1, 0)]);
}

#[test]
fn no_upstream_is_unknown() {
    let fx = Fx::basic();
    fx.ws("stale", false);
    fx.expect(&[], 3, &[line("stale", 1, "unknown", "no-upstream"), tail(1, 1)]);
}

#[test]
fn missing_manifest_is_unknown_exit_3() {
    let fx = Fx::basic();
    fx.lane(2, "other", "r");
    fx.bound(2, "other", FAR);
    fx.ws("other", true);
    fx.expect(&[], 3, &[line("stale", 1, "unknown", "clean-check-failed"), line("other", 2, "stale", "gone-clean-pushed"), tail(2, 1)]);
}

#[test]
fn repo_id_without_repo_map_is_unknown() {
    let fx = Fx::new();
    fx.lane(1, "stale", "zzz");
    fx.bound(1, "stale", FAR);
    fx.expect(&[], 3, &[line("stale", 1, "unknown", "no-repo-map"), tail(1, 1)]);
}

#[test]
fn manifest_that_is_a_fifo_is_unknown_within_the_limit() {
    let fx = Fx::basic();
    fx.fifo(&fx.md.join("stale.jsonl"));
    let before = snapshot(&fx.lanes);
    let started = std::time::Instant::now();
    let o = run_bounded(&fx.args(), 60);
    assert!(started.elapsed() < Duration::from_secs(60));
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().collect::<Vec<_>>(), [line("stale", 1, "unknown", "clean-check-failed"), tail(1, 1)]);
    assert_eq!(o.status.code(), Some(3));
    assert_eq!(snapshot(&fx.lanes), before);
}

#[test]
fn the_run_calls_the_host_for_the_listing_only() {
    let fx = Fx::basic();
    let tree = fx.ws("stale", true);
    let state = |p: &Path| (git(p, &["rev-parse", "HEAD"]), git(p, &["for-each-ref"]), git(p, &["status", "--porcelain"]));
    let (tree_before, repo_before) = (state(&tree), state(&fx.repo));
    fx.expect(&[], 0, &stale_one());
    assert_eq!(fs::read_to_string(&fx.calls).unwrap(), "agents --json --all\n");
    assert_eq!(state(&tree), tree_before);
    assert_eq!(state(&fx.repo), repo_before);
}

#[test]
fn the_lanes_dir_lock_is_free_while_the_host_runs() {
    let fx = Fx::basic();
    fx.ws("stale", true);
    fx.mode("lock");
    let before = snapshot(&fx.lanes);
    let o = run_bounded(&fx.args(), 20);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().collect::<Vec<_>>(), stale_one());
    assert_eq!(fs::read_to_string(&fx.calls).unwrap(), "list-exit=0\n");
    assert_eq!(snapshot(&fx.lanes), before, "the lanes dir changed");
}

// ---------------------------------------------------------------------------------------------- whole-run failures

#[test]
fn host_missing() {
    let fx = Fx::basic();
    let nohost = fx.root.join("nohost");
    fx.refuse(&fx.with("--host", nohost.to_str().unwrap()), "--host", 30);
}

#[test]
fn listing_exits_non_zero() {
    let fx = Fx::basic();
    fx.mode("fail");
    fx.refuse(&fx.args(), "exited non-zero", 30);
}

#[test]
fn listing_hangs() {
    let fx = Fx::basic();
    fx.mode("hang");
    fx.refuse(&fx.args(), "timed out", 30);
}

#[test]
fn listing_not_json() {
    let fx = Fx::basic();
    fx.mode("junk");
    fx.refuse(&fx.args(), "not JSON", 30);
}

#[test]
fn listing_cut_at_the_read_cap() {
    let fx = Fx::basic();
    fs::write(&fx.list, format!("[{{\"id\":\"a1\",\"name\":\"{}\",\"state\":\"working\"}}]\n", "x".repeat((1 << 20) + 4096))).unwrap();
    fx.refuse(&fx.args(), "not JSON", 30);
}

#[test]
fn listing_entry_without_id() {
    let fx = Fx::basic();
    fx.entries(&["{\"name\":\"x\"}".to_string()]);
    fx.refuse(&fx.args(), "no string id and name", 30);
}

#[test]
fn listing_entry_id_not_plain() {
    let fx = Fx::basic();
    fx.entries(&[ent("a b", "x", Some("working"))]);
    fx.refuse(&fx.args(), "not a plain token", 30);
}

#[test]
fn launch_records_missing() {
    let fx = Fx::basic();
    let p = fx.root.join("nolaunches");
    fx.refuse(&fx.with("--launch-records", p.to_str().unwrap()), "missing", 30);
}

#[test]
fn launch_records_fifo_without_writer() {
    let fx = Fx::basic();
    let p = fx.root.join("fifo.jsonl");
    fx.fifo(&p);
    fx.refuse(&fx.with("--launch-records", p.to_str().unwrap()), "not a regular file", 20);
}

#[test]
fn launch_records_symlink() {
    let fx = Fx::basic();
    let p = fx.root.join("link.jsonl");
    symlink(&fx.records, &p).unwrap();
    fx.refuse(&fx.with("--launch-records", p.to_str().unwrap()), "link", 30);
}

#[test]
fn launch_records_torn_tail() {
    let fx = Fx::basic();
    fx.append("{\"v\":1");
    fx.refuse(&fx.args(), "torn", 30);
}

#[test]
fn config_fifo_without_writer() {
    let fx = Fx::basic();
    let p = fx.root.join("fifo.yaml");
    fx.fifo(&p);
    fx.refuse(&fx.with("--config", p.to_str().unwrap()), "config", 20);
}

#[test]
fn lanes_dir_symlink() {
    let fx = Fx::basic();
    let p = fx.root.join("lanes-link");
    symlink(&fx.lanes, &p).unwrap();
    fx.refuse(&fx.with("--lanes-dir", p.to_str().unwrap()), "lanes dir", 30);
}

#[test]
fn manifest_dir_symlink() {
    let fx = Fx::basic();
    let p = fx.root.join("md-link");
    symlink(&fx.md, &p).unwrap();
    fx.refuse(&fx.with("--manifest-dir", p.to_str().unwrap()), "--manifest-dir must be an existing real directory", 30);
}

#[test]
fn repo_map_dir_symlink() {
    let fx = Fx::basic();
    let p = fx.root.join("repo-link");
    symlink(&fx.repo, &p).unwrap();
    let mut a = fx.args();
    let i = a.iter().position(|x| x == "--repo-map").unwrap();
    a[i + 1] = format!("r={}", p.display());
    fx.refuse(&a, "--repo-map must be an existing real directory", 30);
}

#[test]
fn path_spellings_are_refused() {
    let fx = Fx::basic();
    let link = fx.root.join("dir-link");
    symlink(&fx.root, &link).unwrap();
    let cases: [(&str, PathBuf, bool); 6] = [
        ("--lanes-dir", fx.lanes.clone(), true),
        ("--manifest-dir", fx.md.clone(), true),
        ("--repo-map", fx.repo.clone(), true),
        ("--launch-records", fx.records.clone(), false),
        ("--host", fx.host.clone(), false),
        ("--config", fx.cfg.clone(), false),
    ];
    for (opt, p, is_dir) in &cases {
        let s = p.to_str().unwrap();
        let mut spellings = vec![format!("{s}/"), format!("{s}/."), format!("{}/x/../{}", fx.root.display(), p.strip_prefix(&fx.root).unwrap().display())];
        if *is_dir {
            spellings.push(format!("{}/", link.display()));
        }
        for sp in spellings {
            let val = if *opt == "--repo-map" { format!("r={sp}") } else { sp.clone() };
            let mut a = fx.args();
            if *opt == "--repo-map" {
                let i = a.iter().position(|x| x == "--repo-map").unwrap();
                a[i + 1] = val;
            } else {
                a = fx.with(opt, &val);
            }
            fx.refuse(&a, &format!("{opt} must be an absolute path"), 30);
        }
    }
}

#[test]
fn repo_map_twice() {
    let fx = Fx::basic();
    let mut a = fx.args();
    a.extend(["--repo-map".to_string(), format!("r={}", fx.root.display())]);
    fx.refuse(&a, "twice", 30);
}

#[test]
fn repo_map_relative() {
    let fx = Fx::basic();
    let mut a = fx.args();
    let i = a.iter().position(|x| x == "--repo-map").unwrap();
    a[i + 1] = "r=rel/dir".to_string();
    fx.refuse(&a, "--repo-map must be an absolute path", 30);
}

#[test]
fn allow_bad_spelling() {
    let fx = Fx::basic();
    for bad in ["/x", "x/", "a/../b", "a//b", "a b"] {
        let mut a = fx.args();
        a.extend(["--allow".to_string(), bad.to_string()]);
        fx.refuse(&a, "--allow must be a tree-relative path", 30);
    }
}

#[test]
fn unknown_missing_and_repeated_options_are_exit_2() {
    let fx = Fx::basic();
    let mut a = fx.args();
    a.extend(["--bogus".to_string(), "x".to_string()]);
    fx.refuse(&a, "unknown option", 30);
    let mut a = fx.args();
    a.truncate(a.len() - 2);
    fx.refuse(&a, "--repo-map is required", 30);
    let mut a = fx.args();
    a.extend(["--host".to_string(), fx.host.to_str().unwrap().to_string()]);
    fx.refuse(&a, "given twice", 30);
}

#[test]
fn host_given_as_a_link_is_accepted() {
    let fx = Fx::basic();
    fx.ws("stale", true);
    let p = fx.root.join("host-link");
    symlink(&fx.host, &p).unwrap();
    let mut a = fx.with("--host", p.to_str().unwrap());
    a.push("--allow".to_string());
    a.push("x".to_string());
    let before = snapshot(&fx.lanes);
    let o = run_bounded(&a, 120);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().collect::<Vec<_>>(), stale_one());
    assert_eq!(snapshot(&fx.lanes), before, "the lanes dir changed");
}
