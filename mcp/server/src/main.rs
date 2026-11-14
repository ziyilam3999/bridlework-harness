//! Read-only MCP stdio server for the harness CLI (newline-delimited JSON-RPC 2.0).
//! The tool list is embedded from mcp/tools.json; only one tool is wired in this build.

mod json;

use json::Value;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

const TOOLS_JSON: &str = include_str!("../../tools.json");
const PROTOCOL_VERSION: &str = "2025-06-18";
const WIRED_TOOL: &str = "harness_version";
const USAGE: &str = "usage: harness-mcp --harness <absolute-path>";

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

struct Server {
    rows: Vec<Value>,
    harness: String,
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

    fn spawn(&self, row: &Value) -> Result<Value, ()> {
        let argv: Vec<&str> = row
            .get("argv")
            .and_then(Value::as_array)
            .ok_or(())?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mut cmd = Command::new(&self.harness);
        cmd.args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("HARNESS_") {
                cmd.env_remove(&name);
            }
        }
        let out = cmd.output().map_err(|_| ())?;
        let exit = out.status.code().unwrap_or(-1);
        let structured = obj(vec![
            ("exit", Value::Num(exit.to_string())),
            ("stdout", Value::Str(String::from_utf8_lossy(&out.stdout).into_owned())),
            ("stderr", Value::Str(String::from_utf8_lossy(&out.stderr).into_owned())),
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
            ("isError", Value::Bool(false)),
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
        if name != WIRED_TOOL {
            return error(id, -32602, &format!("Tool not wired in this build: {name}"));
        }
        let args_ok = match params.and_then(|p| p.get("arguments")) {
            None => true,
            Some(Value::Obj(pairs)) => pairs.is_empty(),
            Some(_) => false,
        };
        if !args_ok {
            return error(id, -32602, "Invalid arguments");
        }
        match self.spawn(row) {
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

fn parse_args() -> Result<String, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 || args[0] != "--harness" {
        return Err(USAGE.to_string());
    }
    let path = &args[1];
    if !path.starts_with('/') {
        return Err(format!("{USAGE}\nharness path must be absolute"));
    }
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => Ok(path.clone()),
        _ => Err(format!("{USAGE}\nharness path is not a regular file")),
    }
}

fn main() {
    let harness = match parse_args() {
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
    let server = Server { rows, harness };
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
