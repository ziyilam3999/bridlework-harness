//! Command line. Exit 0 clean, 1 hit, 2 error (nothing on stdout when erroring).
use crate::classes;
use crate::explain;
use crate::gitio;
use crate::overlay;
use crate::scan::{scan_text, Ctx, Hit};
use std::io::Read;

fn take_testmode(args: &mut Vec<String>) -> bool {
    let mut on = false;
    if let Some(i) = args.iter().position(|a| a == "--testmode") {
        args.remove(i);
        if i < args.len() {
            on = args.remove(i) == "synthetic-test";
        }
    }
    on
}

fn take_surface(args: &mut Vec<String>) -> Result<String, String> {
    let mut surface = "text".to_string();
    if let Some(i) = args.iter().position(|a| a == "--surface") {
        args.remove(i);
        if i >= args.len() {
            return Err("missing surface".to_string());
        }
        surface = args.remove(i);
    }
    if surface.is_empty() || !surface.bytes().all(|c| c.is_ascii_lowercase() || c == b'_' || c == b'.') {
        return Err("bad surface".to_string());
    }
    Ok(surface)
}

struct Outcome {
    hits: Vec<Hit>,
    stderr: Vec<String>,
    text_mode: bool,
}

fn run_scan(args: &[String], testmode: bool, surface: &str) -> Result<Outcome, String> {
    let list = overlay::load()?;
    let ctx = Ctx { list: &list, testmode };
    let mode = args.first().map(|s| s.as_str()).ok_or("no mode given")?;
    let mut stderr = Vec::new();
    let mut text_mode = false;
    let hits = match mode {
        "--staged" => gitio::staged(&ctx)?,
        "--range" => {
            let (h, n) = gitio::range(&ctx, &args[1..])?;
            stderr.push(format!("scanned_commits={}", n));
            h
        }
        "--tag" => gitio::tag(&ctx, args.get(1).ok_or("no tag object")?)?,
        "--tree" => {
            let (h, n) = gitio::tree(&ctx)?;
            stderr.push(format!("files={}", n));
            h
        }
        "--text" => {
            text_mode = true;
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf).map_err(|_| "stdin unreadable".to_string())?;
            let t = String::from_utf8(buf).map_err(|_| "invalid utf-8".to_string())?;
            scan_text(&ctx, surface, &t)
        }
        _ => return Err("unknown mode".to_string()),
    };
    Ok(Outcome { hits, stderr, text_mode })
}

pub fn run(mut args: Vec<String>) -> i32 {
    if args.len() == 1 && args[0] == "--version" {
        println!("{}", crate::version_line());
        return 0;
    }
    let testmode = take_testmode(&mut args);
    if args.len() == 2 && args[0] == "privacy" && args[1] == "classes" {
        for c in classes::ALL {
            println!("{}", c);
        }
        return 0;
    }
    if args.len() < 3 || args[0] != "privacy" || (args[1] != "scan" && args[1] != "explain") {
        eprintln!("usage: harness --version | privacy classes | privacy scan|explain <mode>");
        return 2;
    }
    let explain_mode = args[1] == "explain";
    let mut rest: Vec<String> = args[2..].to_vec();
    let surface = match take_surface(&mut rest) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("harness: error: {}", e);
            return 2;
        }
    };
    match run_scan(&rest, testmode, &surface) {
        Err(e) => {
            eprintln!("harness: error: {}", e);
            2
        }
        Ok(o) => {
            for l in &o.stderr {
                eprintln!("{}", l);
            }
            for h in &o.hits {
                let line = if explain_mode {
                    explain::explain_line(h, o.text_mode)
                } else if o.text_mode {
                    explain::text_line(h)
                } else {
                    explain::hit_line(h)
                };
                println!("{}", line);
            }
            if o.hits.is_empty() {
                0
            } else {
                1
            }
        }
    }
}
