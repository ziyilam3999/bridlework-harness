//! The class ids the engine reports. Exactly nine.

pub const ALL: [&str; 9] = [
    "deny-token",
    "email",
    "home-path",
    "credential",
    "session-link",
    "coauthor-personal",
    "regulated-id",
    "baseline-number",
    "synthetic-test",
];

pub fn is_class(id: &str) -> bool {
    ALL.contains(&id)
}
