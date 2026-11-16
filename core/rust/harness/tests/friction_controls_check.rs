//! `harness friction controls-check`, process-level on scratch files. Every id and name is synthetic.
#![cfg(unix)]
mod friction_common;
use friction_common::*;

/// The inbox of the worked case: g-1 (blocker) broken once, g-2 seen once, g-3 broken once.
fn worked(t: &Tmp) {
    t.write_inbox(&[
        hand("g-1", "2026-11-02", "", "blocker"),
        hand("g-1b", "2026-11-09", "g-1", ""),
        hand("g-2", "2026-11-02", "", ""),
        hand("g-3", "2026-11-02", "", ""),
        hand("g-3b", "2026-11-09", "g-3", ""),
    ]);
}

#[test]
fn rank_output_of_the_worked_inbox_is_unchanged() {
    let t = Tmp::new();
    worked(&t);
    let cfg = t.config();
    let o = run(&["friction", "rank", "--top", "100000", "--config", p(&cfg)]);
    assert_eq!(code(&o), 0);
    assert_eq!(
        out(&o),
        "rank group=g-1 score=54 impact=blocker occurrences=2 first=2026-11-02 last=2026-11-09\n\
         rank group=g-3 score=2 impact=unrated occurrences=2 first=2026-11-02 last=2026-11-09\n\
         rank group=g-2 score=1 impact=unrated occurrences=1 first=2026-11-02 last=2026-11-02\n\
         rank ok groups=3 listed=3\n"
    );
}
