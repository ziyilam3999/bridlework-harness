//! Binary entry. Reads argv without panicking on non-UTF-8 bytes: the first offending argument is
//! refused with one stderr line naming its 1-based position (counted from the first argument after
//! the program name) and exit 2 — the crate's refusal code — instead of the exit-101 panic
//! `std::env::args` would raise. Valid argv reaches `cli::run` exactly as before.

fn main() {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let args = match utf8_argv(&argv) {
        Ok(args) => args,
        Err(position) => {
            eprintln!("harness: error: argument {} is not valid UTF-8", position);
            std::process::exit(2);
        }
    };
    std::process::exit(harness::cli::run(args));
}

/// Convert argv to `String`s, reporting the 1-based position of the first argument that is not valid
/// UTF-8 (the raw bytes stay out of the message: they are not echoed, not even lossily).
fn utf8_argv(argv: &[std::ffi::OsString]) -> Result<Vec<String>, usize> {
    argv.iter()
        .enumerate()
        .map(|(i, a)| a.to_str().map(|s| s.to_string()).ok_or(i + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::utf8_argv;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    fn os(items: &[&str]) -> Vec<OsString> {
        items.iter().map(|s| OsString::from(*s)).collect()
    }

    #[test]
    fn valid_argv_passes_through_in_order() {
        let argv = os(&["events", "list", "--log", "a.jsonl"]);
        assert_eq!(utf8_argv(&argv), Ok(vec!["events".into(), "list".into(), "--log".into(), "a.jsonl".into()]));
    }

    #[test]
    fn empty_argv_is_ok_empty() {
        assert_eq!(utf8_argv(&[]), Ok(Vec::new()));
    }

    #[test]
    fn first_bad_position_is_reported_one_based() {
        // Two bad arguments (positions 3 and 5): the first one wins, counted from the first
        // argument after the program name.
        let mut argv = os(&["events", "list"]);
        argv.push(OsString::from_vec(vec![0xFF, 0xFE]));
        argv.push(OsString::from("ticket"));
        argv.push(OsString::from_vec(vec![0xFF]));
        assert_eq!(utf8_argv(&argv), Err(3));
    }
}
