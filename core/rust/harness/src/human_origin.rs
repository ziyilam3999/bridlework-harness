//! The ONE human-origin check (std only, unix only). A write to a file that decides what agents may do (the
//! config; later the permissions and the control registry) is accepted only with a human approval.
//!
//! The proof is a detached OpenSSH signature (`ssh-keygen -Y sign`, namespace `bridlework-human-origin`) over a
//! message this module builds itself from the exact write (target, file name, expiry, the full new content). It is
//! checked by `/usr/bin/ssh-keygen -Y verify` against an `allowed_signers` file at a compiled-in path that only root
//! can change. Every step fails closed: the first failure is the refusal.
//!
//! Why an agent cannot forge it: it cannot sign (it has no access to the human's private key, which needs a touch or
//! a passphrase); it cannot swap the trusted key (the anchor and every folder above it must be owned by root and not
//! group/other writable, and in release builds the anchor path is a constant with no flag or env var); it cannot
//! reuse an approval for other content (the content, target, name and expiry are inside the signed message).
//!
//! A `Refusal` is a closed enum with a fixed word per reason; it never carries a value, a path or helper output.

use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The compiled-in trust anchor.
pub const ANCHOR_PATH: &str = "/etc/bridlework/human-origin.allowed_signers";
/// The principal the anchor must name.
pub const PRINCIPAL: &str = "bridlework-human";
/// The signature namespace.
pub const NAMESPACE: &str = "bridlework-human-origin";
/// The longest approval window: one hour.
pub const WINDOW_CAP_MS: u64 = 3_600_000;
/// The absolute path of the verifier.
const VERIFIER: &str = "/usr/bin/ssh-keygen";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Config,
    Permission,
    ControlRegistry,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Config => "config",
            Target::Permission => "permission",
            Target::ControlRegistry => "control_registry",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    AnchorUntrusted,
    NoApproval,
    Expired,
    WindowTooLong,
    SignatureInvalid,
    VerifierUnavailable,
}

impl Refusal {
    pub const ALL: [Refusal; 6] =
        [Refusal::AnchorUntrusted, Refusal::NoApproval, Refusal::Expired, Refusal::WindowTooLong, Refusal::SignatureInvalid, Refusal::VerifierUnavailable];

    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::AnchorUntrusted => "anchor_untrusted",
            Refusal::NoApproval => "no_approval",
            Refusal::Expired => "expired",
            Refusal::WindowTooLong => "window_too_long",
            Refusal::SignatureInvalid => "signature_invalid",
            Refusal::VerifierUnavailable => "verifier_unavailable",
        }
    }
}

/// Where the trusted keys live. Built only by `system()` or, in debug builds, `for_test`.
pub struct Anchor {
    path: PathBuf,
    test: bool,
}

impl Anchor {
    pub fn system() -> Anchor {
        Anchor { path: PathBuf::from(ANCHOR_PATH), test: false }
    }

    /// Debug builds only: a scratch anchor, owned by root or the current user, with only its own folder checked.
    #[cfg(debug_assertions)]
    pub fn for_test(path: &Path) -> Anchor {
        Anchor { path: path.to_path_buf(), test: true }
    }
}

/// The anchor file's own verdict. Pure.
pub fn anchor_verdict(is_symlink: bool, is_file: bool, uid: u32, mode: u32, allow_uid: Option<u32>) -> Result<(), Refusal> {
    if is_symlink || !is_file {
        return Err(Refusal::AnchorUntrusted);
    }
    folder_or_file_verdict(uid, mode, allow_uid)
}

fn folder_or_file_verdict(uid: u32, mode: u32, allow_uid: Option<u32>) -> Result<(), Refusal> {
    if uid != 0 && Some(uid) != allow_uid {
        return Err(Refusal::AnchorUntrusted);
    }
    if mode & 0o022 != 0 {
        return Err(Refusal::AnchorUntrusted);
    }
    Ok(())
}

/// The approval window verdict. Pure.
pub fn window_verdict(now: u64, expires: u64, cap: u64) -> Result<(), Refusal> {
    if now > expires {
        return Err(Refusal::Expired);
    }
    if expires - now > cap {
        return Err(Refusal::WindowTooLong);
    }
    Ok(())
}

/// The exact message the human signs.
pub fn message(target: Target, name: &str, expires: u64, content: &[u8]) -> Vec<u8> {
    let mut m = format!("bridlework-human-origin v1\ntarget={}\nname={}\nexpires_ms={}\ncontent-follows\n", target.as_str(), name, expires).into_bytes();
    m.extend_from_slice(content);
    m
}

fn anchor_trusted(a: &Anchor) -> Result<(), Refusal> {
    let allow = if a.test { Some(crate::death::my_uid().map_err(|_| Refusal::AnchorUntrusted)?) } else { None };
    let md = std::fs::symlink_metadata(&a.path).map_err(|_| Refusal::AnchorUntrusted)?;
    anchor_verdict(md.file_type().is_symlink(), md.is_file(), md.uid(), md.mode(), allow)?;
    // Ancestors: followed (a symlinked ancestor such as macOS /etc is fine); owner and mode only.
    let mut cur = a.path.parent();
    while let Some(dir) = cur {
        if dir.as_os_str().is_empty() {
            break;
        }
        let m = std::fs::metadata(dir).map_err(|_| Refusal::AnchorUntrusted)?;
        folder_or_file_verdict(m.uid(), m.mode(), allow)?;
        if a.test {
            break;
        }
        cur = dir.parent();
    }
    Ok(())
}

/// The ONE check. Ok means a human approved exactly this write.
pub fn check(target: Target, name: &str, content: &[u8], approval: Option<&Path>, expires_ms: Option<u64>, now_ms: u64, anchor: &Anchor) -> Result<(), Refusal> {
    anchor_trusted(anchor)?;
    let (Some(sig), Some(expires)) = (approval, expires_ms) else { return Err(Refusal::NoApproval) };
    window_verdict(now_ms, expires, WINDOW_CAP_MS)?;
    let msg = message(target, name, expires, content);
    let mut child = Command::new(VERIFIER)
        .args(["-Y", "verify", "-f"])
        .arg(&anchor.path)
        .args(["-I", PRINCIPAL, "-n", NAMESPACE, "-s"])
        .arg(sig)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Refusal::VerifierUnavailable)?;
    if let Some(mut si) = child.stdin.take() {
        // A verifier that exits early closes the pipe; its exit status decides, not this write.
        let _ = si.write_all(&msg);
    }
    match child.wait() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(Refusal::SignatureInvalid),
        Err(_) => Err(Refusal::VerifierUnavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_verdict_table() {
        assert!(anchor_verdict(false, true, 0, 0o644, None).is_ok());
        assert!(anchor_verdict(false, true, 501, 0o644, None).is_err());
        assert!(anchor_verdict(false, true, 501, 0o644, Some(501)).is_ok());
        assert!(anchor_verdict(false, true, 0, 0o664, None).is_err());
        assert!(anchor_verdict(false, true, 0, 0o646, None).is_err());
        assert!(anchor_verdict(true, true, 0, 0o644, None).is_err());
        assert!(anchor_verdict(false, false, 0, 0o644, None).is_err());
    }

    #[test]
    fn window_verdict_boundaries() {
        assert!(window_verdict(100, 100, 50).is_ok());
        assert!(window_verdict(100, 150, 50).is_ok());
        assert_eq!(window_verdict(100, 151, 50), Err(Refusal::WindowTooLong));
        assert_eq!(window_verdict(101, 100, 50), Err(Refusal::Expired));
    }

    #[test]
    fn message_layout_golden() {
        let m = message(Target::Config, "h.yaml", 42, b"a\nb");
        assert_eq!(m, b"bridlework-human-origin v1\ntarget=config\nname=h.yaml\nexpires_ms=42\ncontent-follows\na\nb".to_vec());
    }

    #[test]
    fn refusal_words_are_fixed() {
        for r in Refusal::ALL {
            let w = r.as_str();
            assert!(!w.is_empty() && !w.contains('/') && w.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }
}
