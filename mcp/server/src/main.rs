//! Read-only MCP stdio server for the harness CLI (newline-delimited JSON-RPC 2.0).
//! The tool list is embedded from mcp/tools.json; each call's inputs are mapped to argv by args.rs.

mod args;
mod json;

use json::Value;
use std::io::{BufRead, Read, Write};
use std::process::{Command, Stdio};

const TOOLS_JSON: &str = include_str!("../../tools.json");
const PROTOCOL_VERSION: &str = "2025-06-18";
const USAGE: &str = "usage: harness-mcp --harness <absolute-path> [--privacy-list <absolute-path>]";
/// Per-stream cap on what a child's output may put in a reply.
pub const CAP_BYTES: usize = 262144;

fn s(text: &str) -> Value {
    Value::Str(text.to_string())
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn error(id: Value, code: i32, message: &str) -> Value {
    obj(vec![
        ("jsonrpc", s("2.0")),
        ("id", id),
        (
            "error",
            obj(vec![("code", Value::Num(code.to_string())), ("message", s(message))]),
        ),
    ])
}

fn result(id: Value, res: Value) -> Value {
    obj(vec![("jsonrpc", s("2.0")), ("id", id), ("result", res)])
}

/// Read a pipe to EOF, keeping the first CAP_BYTES; the rest is read and dropped so the child never blocks.
fn capped(mut pipe: impl Read) -> (Vec<u8>, bool) {
    let mut kept: Vec<u8> = Vec::new();
    let mut cut = false;
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = CAP_BYTES - kept.len();
                if n > room {
                    cut = true;
                }
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    (kept, cut)
}

/// Text of a stream; a cut stream ends at a full UTF-8 char and carries the fixed marker.
fn render(buf: &[u8], cut: bool) -> String {
    if !cut {
        return String::from_utf8_lossy(buf).into_owned();
    }
    let end = match std::str::from_utf8(buf) {
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        _ => buf.len(),
    };
    format!(
        "{}\n[harness-mcp: output truncated at {CAP_BYTES} bytes]\n",
        String::from_utf8_lossy(&buf[..end])
    )
}

struct Server {
    rows: Vec<Value>,
    harness: String,
    privacy_list: Option<String>,
}

impl Server {
    fn row(&self, name: &str) -> Option<&Value> {
        self.rows
            .iter()
            .find(|r| r.get("name").and_then(Value::as_str) == Some(name))
    }

    fn list(&self) -> Value {
        let tools = self
            .rows
            .iter()
            .map(|r| {
                let mut pairs = Vec::new();
                for key in ["name", "description", "inputSchema", "outputSchema"] {
                    if let Some(v) = r.get(key) {
                        pairs.push((key.to_string(), v.clone()));
                    }
                }
                pairs.push((
                    "annotations".to_string(),
                    obj(vec![("readOnlyHint", Value::Bool(true))]),
                ));
                Value::Obj(pairs)
            })
            .collect();
        obj(vec![("tools", Value::Arr(tools))])
    }

    fn spawn(&self, name: &str, argv: Vec<String>, stdin_text: Option<String>) -> Result<Value, ()> {
        let mut cmd = Command::new(&self.harness);
        cmd.args(argv)
            .stdin(if stdin_text.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (var, _) in std::env::vars_os() {
            if var.to_string_lossy().starts_with("HARNESS_") {
                cmd.env_remove(&var);
            }
        }
        if let (true, Some(list)) = (args::needs_privacy_list(name), &self.privacy_list) {
            cmd.env("HARNESS_PRIVACY_LIST", list);
        }
        let mut child = cmd.spawn().map_err(|_| ())?;
        let writer = match (child.stdin.take(), stdin_text) {
            (Some(mut pipe), Some(text)) => Some(std::thread::spawn(move || {
                // A child that exits without reading gives a write error; that is not ours to report.
                let _ = pipe.write_all(text.as_bytes());
            })),
            _ => None,
        };
        let out_t = child.stdout.take().map(|p| std::thread::spawn(move || capped(p)));
        let err_t = child.stderr.take().map(|p| std::thread::spawn(move || capped(p)));
        let status = child.wait().map_err(|_| ())?;
        if let Some(w) = writer {
            let _ = w.join();
        }
        let (stdout, out_cut) = out_t.and_then(|t| t.join().ok()).unwrap_or_default();
        let (stderr, err_cut) = err_t.and_then(|t| t.join().ok()).unwrap_or_default();
        let exit = status.code().unwrap_or(-1);
        let structured = obj(vec![
            ("exit", Value::Num(exit.to_string())),
            ("stdout", Value::Str(render(&stdout, out_cut))),
            ("stderr", Value::Str(render(&stderr, err_cut))),
        ]);
        Ok(obj(vec![
            (
                "content",
                Value::Arr(vec![obj(vec![
                    ("type", s("text")),
                    ("text", Value::Str(json::write(&structured))),
                ])]),
            ),
            ("structuredContent", structured),
            ("isError", Value::Bool(out_cut || err_cut)),
        ]))
    }

    fn call(&self, id: Value, params: Option<&Value>) -> Value {
        let name = match params.and_then(|p| p.get("name")).and_then(Value::as_str) {
            Some(n) => n,
            None => return error(id, -32602, "Invalid params"),
        };
        let row = match self.row(name) {
            Some(r) => r,
            None => return error(id, -32602, &format!("Unknown tool: {name}")),
        };
        let (argv, stdin_text) = match args::build(row, params.and_then(|p| p.get("arguments"))) {
            Ok(built) => built,
            Err(m) => return error(id, -32602, &format!("Invalid arguments: {m}")),
        };
        match self.spawn(name, argv, stdin_text) {
            Ok(res) => result(id, res),
            Err(()) => error(id, -32603, "Spawn failed"),
        }
    }

    /// Answer one input line; `None` means no output (blank line or notification).
    fn handle(&self, line: &str) -> Option<Value> {
        if line.trim().is_empty() {
            return None;
        }
        let msg = match json::parse(line) {
            Ok(v) => v,
            Err(_) => return Some(error(Value::Null, -32700, "Parse error")),
        };
        let id = match msg.get("id") {
            Some(v @ (Value::Str(_) | Value::Num(_))) => Some(v.clone()),
            _ => None,
        };
        let has_id_key = msg.get("id").is_some();
        let method = msg.get("method").and_then(Value::as_str);
        let valid = matches!(msg, Value::Obj(_))
            && msg.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && method.is_some();
        if !valid || (has_id_key && id.is_none()) {
            return Some(error(id.unwrap_or(Value::Null), -32600, "Invalid Request"));
        }
        let id = id?;
        let method = method?;
        Some(match method {
            "initialize" => result(
                id,
                obj(vec![
                    ("protocolVersion", s(PROTOCOL_VERSION)),
                    ("capabilities", obj(vec![("tools", obj(vec![]))])),
                    (
                        "serverInfo",
                        obj(vec![("name", s("harness-mcp")), ("version", s("0.1.0"))]),
                    ),
                ]),
            ),
            "ping" => result(id, obj(vec![])),
            "tools/list" => result(id, self.list()),
            "tools/call" => self.call(id, msg.get("params")),
            _ => error(id, -32601, "Method not found"),
        })
    }
}

fn checked_file(label: &str, path: &str) -> Result<String, String> {
    if !path.starts_with('/') {
        return Err(format!("{USAGE}\n{label} path must be absolute"));
    }
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => Ok(path.to_string()),
        _ => Err(format!("{USAGE}\n{label} path is not a regular file")),
    }
}

fn parse_args() -> Result<(String, Option<String>), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut harness: Option<String> = None;
    let mut list: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).ok_or_else(|| USAGE.to_string())?;
        match args[i].as_str() {
            "--harness" if harness.is_none() => harness = Some(checked_file("harness", value)?),
            "--privacy-list" if list.is_none() => list = Some(checked_file("privacy list", value)?),
            _ => return Err(USAGE.to_string()),
        }
        i += 2;
    }
    Ok((harness.ok_or_else(|| USAGE.to_string())?, list))
}

fn main() {
    let (harness, privacy_list) = match parse_args() {
        Ok(h) => h,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let rows = match json::parse(TOOLS_JSON)
        .ok()
        .and_then(|v| v.get("tools").and_then(Value::as_array).map(<[Value]>::to_vec))
    {
        Some(r) => r,
        None => {
            eprintln!("harness-mcp: embedded tool list is unreadable");
            std::process::exit(2);
        }
    };
    let server = Server { rows, harness, privacy_list };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("harness-mcp: read error: {e}");
                break;
            }
        };
        if let Some(reply) = server.handle(&line) {
            if writeln!(out, "{}", json::write(&reply)).is_err() || out.flush().is_err() {
                break;
            }
        }
    }
}
