//! The repo id (std only). A ticket names exactly one repository by a plain name; this is the one place that decides what a plain name is.
//! It says which repository, never where it lives: an id is never a path, so nothing here touches the filesystem.

/// A validated repo id: lowercase letters, digits, `-`, `_` and `.`, never empty, never `.` or `..`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId(String);

impl RepoId {
    /// Accept a plain name, refuse anything else. The refusal names `repo_id`.
    pub fn parse(s: &str) -> Result<RepoId, String> {
        let plain = !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
        if plain {
            Ok(RepoId(s.to_string()))
        } else {
            Err("`repo_id` must be a plain name (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')".to_string())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_id_that_is_a_plain_name_is_accepted() {
        for name in ["my-repo", "scratch.1", "a_b"] {
            let id = RepoId::parse(name).unwrap_or_else(|e| panic!("{name} refused: {e}"));
            assert_eq!(id.as_str(), name, "the id comes back as given");
        }
    }

    #[test]
    fn a_malformed_repo_id_is_refused() {
        for bad in ["", " ", "Repo", "/abs", "a/b", "..", ".", "a\\b", "a\0b", "my repo", "repo\n"] {
            let err = RepoId::parse(bad).expect_err(&format!("{bad:?} must be refused"));
            assert!(err.contains("`repo_id`"), "the refusal names the field: {err}");
        }
    }
}
