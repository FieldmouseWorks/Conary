use super::TRUNK;
use super::flow_ci_case::*;
use crate::flow::*;
fn legs(suite: u64, n: u32, state: &str, gate: Option<&str>, base: &str) -> Run {
    let mut r = Run::new(
        suite,
        &[
            (&format!("check / lane into {base}"), "success"),
            ("check / commits", "success"),
        ],
    );
    for k in 1..=n {
        r = r.add(
            &format!("check / leg {k} of {n}"),
            if matches!(state, "cancelled" | "skipped") || k == 3 {
                state
            } else {
                "success"
            },
        );
    }
    if let Some(g) = gate {
        r = r.add(GATE, g);
    }
    r
}
#[test]
fn legs_wait_for_the_gate_but_a_failed_leg_is_red_immediately() {
    // legs.py:34,39,44,46.
    let s = Scenario::new();
    s.assert(
        &[legs(1, 4, "success", None, "main").going()],
        Ci::Pending,
        &["flake (still running)"],
    );
    s.assert(
        &[legs(1, 4, "success", Some("success"), "main")],
        Ci::Green,
        &[],
    );
    s.assert(
        &[legs(1, 4, "success", Some("failure"), "main")],
        Ci::Red,
        &[GATE],
    );
    s.assert(
        &[legs(1, 4, "failure", None, "main").going()],
        Ci::Red,
        &["check / leg 3 of 4"],
    );
    let r = Run::new(1, &[("scan", "success")]).no_workflow().going();
    s.assert(&[r], Ci::Green, &[]);
}
#[test]
fn only_equivalent_completed_gate_work_replaces_cancelled_legs() {
    // legs.py:51,61: full replacement and nonreplacement matrices, both rollup orders.
    let s = Scenario::new();
    let old = legs(1, 4, "cancelled", Some("skipped"), "main");
    for new in [
        legs(2, 4, "success", Some("success"), "main"),
        legs(2, 3, "success", Some("success"), "main"),
        Run::whole(2, "main", "success"),
        Run::docs(2, TRUNK, "success"),
    ] {
        s.assert(&[old.clone(), new.clone()], Ci::Green, &[]);
        s.assert(&[new, old.clone()], Ci::Green, &[]);
    }
    for new in [
        Run::whole(2, "main", "failure"),
        legs(2, 3, "success", Some("failure"), "main"),
    ] {
        s.assert(&[old.clone(), new], Ci::Red, &[GATE]);
    }
    let mut sibling = Run::new(2, &[("other / lane into main", "success")]);
    for k in 1..=4 {
        sibling = sibling.add(&format!("other / leg {k} of 4"), "success");
    }
    for new in [
        Run::new(
            2,
            &[
                ("check / lane into main", "success"),
                ("check / commits", "success"),
            ],
        ),
        sibling,
        legs(2, 4, "success", Some("success"), "next"),
        legs(2, 4, "skipped", None, "main"),
        legs(2, 4, "cancelled", Some("skipped"), "main"),
    ] {
        s.assert(
            &[old.clone(), new],
            Ci::Pending,
            &[
                "check / leg 1 of 4 (cancelled, to be run again)",
                "check / leg 2 of 4 (cancelled, to be run again)",
                "check / leg 3 of 4 (cancelled, to be run again)",
                "check / leg 4 of 4 (cancelled, to be run again)",
            ],
        );
    }
}
#[test]
fn the_whole_gate_replaces_a_cancelled_leg_of_any_count() {
    // The gate runs up to 12 legs: a later whole gate on the same head
    // stands for every one of them, the eleventh as much as the first.
    let s = Scenario::new();
    let old = legs(1, 12, "cancelled", Some("skipped"), "main");
    s.assert(
        &[old.clone(), Run::whole(2, "main", "success")],
        Ci::Green,
        &[],
    );
    let one = Run::new(
        1,
        &[
            ("check / lane into main", "success"),
            ("check / leg 11 of 12", "cancelled"),
        ],
    );
    s.assert(&[one, Run::whole(2, "main", "success")], Ci::Green, &[]);
}
#[test]
fn a_leg_is_k_of_n_for_any_count_and_nothing_else() {
    let s = Scenario::new();
    let cancelled = |job: &str| {
        Run::new(
            1,
            &[
                ("check / lane into main", "success"),
                (&format!("check / {job}"), "cancelled"),
            ],
        )
    };
    for job in ["leg 9 of 9", "leg 12 of 12", "leg 1 of 2", "leg 11 of 120"] {
        s.assert(
            &[cancelled(job), Run::whole(2, "main", "success")],
            Ci::Green,
            &[],
        );
    }
    for job in [
        "leg 0 of 2",
        "leg 3 of 2",
        "leg 01 of 12",
        "leg 1 of 1",
        "leg 1 of 02",
        "leg +1 of 2",
        "leg 1  of 2",
        "leg -1 of 2",
        "leg 1 of 2 ",
    ] {
        s.assert(
            &[cancelled(job), Run::whole(2, "main", "success")],
            Ci::Pending,
            &[&format!("check / {job} (cancelled, to be run again)")],
        );
    }
}
#[test]
fn one_leg_never_stands_for_another() {
    // upkeep's rule: the same job replaces itself, and only the whole gate
    // (or the docs job, where the docs lane applies) stands for a leg.
    let s = Scenario::new();
    let old = Run::new(
        1,
        &[
            ("check / lane into main", "success"),
            ("check / leg 1 of 4", "cancelled"),
        ],
    );
    let other = Run::new(
        2,
        &[
            ("check / lane into main", "success"),
            ("check / leg 2 of 4", "success"),
        ],
    );
    s.assert(
        &[old.clone(), other],
        Ci::Pending,
        &["check / leg 1 of 4 (cancelled, to be run again)"],
    );
    let same = Run::new(
        2,
        &[
            ("check / lane into main", "success"),
            ("check / leg 1 of 4", "success"),
        ],
    );
    s.assert(&[old, same], Ci::Green, &[]);
}
#[test]
fn a_failed_leg_stays_red_beside_any_later_run() {
    // The flow supersedes cancelled work only: a failed leg is red whatever
    // a later suite on the same head did, whole gate included. (upkeep also
    // lets later work replace a failure; the flow doesn't.)
    let s = Scenario::new();
    let failed = |job: &str| {
        Run::new(
            1,
            &[
                ("check / lane into main", "success"),
                (&format!("check / {job}"), "failure"),
            ],
        )
    };
    let later = Run::new(
        2,
        &[
            ("check / lane into main", "success"),
            ("check / leg 2 of 4", "success"),
        ],
    );
    s.assert(
        &[failed("leg 1 of 4"), later],
        Ci::Red,
        &["check / leg 1 of 4"],
    );
    s.assert(
        &[failed("leg 11 of 12"), Run::whole(2, "main", "success")],
        Ci::Red,
        &["check / leg 11 of 12"],
    );
}
