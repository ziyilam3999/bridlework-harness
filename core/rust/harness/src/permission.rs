//! The permission-set shape check (std only). The new content of a permission write must be a JSON object whose only key is
//! `permissions`; that value is an object whose keys are a subset of `allow`, `deny`, `ask`; each is an array of non-empty strings
//! with no duplicates. Content that fails is refused before the human-origin check runs.

use crate::control::{parse_doc, V};

pub fn parse(text: &str) -> Result<(), String> {
    let doc = parse_doc(text)?;
    let V::O(top) = &doc else { return Err("permission: not a JSON object".to_string()) };
    if top.len() != 1 || top[0].0 != "permissions" {
        return Err("permission: the only top-level key must be `permissions`".to_string());
    }
    let V::O(inner) = &top[0].1 else { return Err("permission: `permissions` must be an object".to_string()) };
    let mut seen: Vec<&str> = Vec::new();
    for (k, v) in inner {
        if !["allow", "deny", "ask"].contains(&k.as_str()) {
            return Err(format!("permission: unknown key `{}` in `permissions`", k));
        }
        if seen.contains(&k.as_str()) {
            return Err(format!("permission: `{}` appears twice", k));
        }
        seen.push(k);
        let V::A(items) = v else { return Err(format!("permission: `{}` must be an array", k)) };
        let mut strs: Vec<&str> = Vec::new();
        for it in items {
            match it {
                V::S(s) if !s.is_empty() => {
                    if strs.contains(&s.as_str()) {
                        return Err(format!("permission: `{}` has a duplicate entry", k));
                    }
                    strs.push(s);
                }
                _ => return Err(format!("permission: `{}` entries must be non-empty strings", k)),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_template_shape() {
        let t = include_str!("../../../../plugins/claude/managed-settings.template.json");
        assert_eq!(parse(t), Ok(()));
    }

    #[test]
    fn refuses_bad_shapes_table() {
        for bad in [
            "[]",
            "{}",
            r#"{"deny":["a"]}"#,
            r#"{"permissions":{"deny":["a"]},"x":1}"#,
            r#"{"permissions":{"block":["a"]}}"#,
            r#"{"permissions":{"deny":"a"}}"#,
            r#"{"permissions":{"deny":[""]}}"#,
            r#"{"permissions":{"deny":["a","a"]}}"#,
            r#"{"permissions":[]}"#,
        ] {
            assert!(parse(bad).is_err(), "must refuse: {bad}");
        }
    }

    #[test]
    fn refuses_trailing_garbage() {
        assert!(parse(r#"{"permissions":{"deny":["a"]}} x"#).is_err());
    }
}
