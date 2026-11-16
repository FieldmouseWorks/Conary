//! The lessons register (`docs/lessons.toml`): the gate reads the real one,
//! and each rule refuses the register that breaks it.

use conary_core::lessons::{Register, RegisterError};

/// The repository's own register, as the gate reads it. Only its rules are
/// checked here, never whether a milestone may exit: an open lesson is an
/// honest record, and recording one mustn't stop the gate.
#[test]
fn the_register_is_valid() {
    let register = Register::parse(include_str!("../../../docs/lessons.toml"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!register.lessons().is_empty());
    for m in ["M6", "M7"] {
        assert!(!register.ships(m).is_empty(), "{m} ships nothing");
    }
}

const HEAD: &str = r#"
milestones = ["M6", "M7"]

[components]
"flow" = ["M6"]
"conaryd" = ["M6", "M7"]
"M7 standalone setup" = ["M7"]
"#;

fn lesson(id: &str, applies: &str, status: &str) -> String {
    format!(
        r#"
[[lessons]]
id = "{id}"
lesson = "One line."
from = ["https://github.com/FieldmouseWorks/burrow/issues/1", "https://github.com/FieldmouseWorks/remi/pull/2"]
applies = [{applies}]
status = "{status}"
where = ["docs/DESIGN.md#properties"]
"#
    )
}

fn register(lessons: &[String]) -> String {
    format!("{HEAD}{}", lessons.concat())
}

/// The problems a register is refused for, or what it failed to parse with.
fn refused(text: &str) -> String {
    match Register::parse(text) {
        Ok(_) => panic!("accepted:\n{text}"),
        Err(e @ (RegisterError::Parse(_) | RegisterError::Invalid(_))) => e.to_string(),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn a_well_formed_register_is_accepted() {
    let text = register(&[
        lesson("a", r#""flow""#, "open"),
        lesson("b", r#""conaryd", "M7 standalone setup""#, "designed"),
        lesson("c-2", r#""flow""#, "built"),
    ]);
    let r = Register::parse(&text).unwrap();
    assert_eq!(r.lessons().len(), 3);
    assert_eq!(r.ships("M7"), ["M7 standalone setup", "conaryd"]);
}

#[test]
fn every_field_is_required() {
    let whole = lesson("a", r#""flow""#, "open");
    for field in ["id", "lesson", "from", "applies", "status", "where"] {
        let without: String = whole
            .lines()
            .filter(|l| !l.starts_with(&format!("{field} =")))
            .map(|l| format!("{l}\n"))
            .collect();
        let e = refused(&register(&[without]));
        assert!(
            e.contains(&format!("missing field `{field}`")),
            "{field}: {e}"
        );
    }
    let without_milestones =
        register(std::slice::from_ref(&whole)).replace(r#"milestones = ["M6", "M7"]"#, "");
    let without_components = format!("milestones = [\"M6\"]\n{whole}");
    for (section, text) in [
        ("milestones", without_milestones),
        ("components", without_components),
    ] {
        let e = refused(&text);
        assert!(
            e.contains(&format!("missing field `{section}`")),
            "{section}: {e}"
        );
    }
    let extra = whole.replace("status =", "owner = \"x\"\nstatus =");
    assert!(refused(&register(&[extra])).contains("unknown field `owner`"));
}

#[test]
fn a_source_must_link_a_github_issue_or_pr() {
    for bad in [
        "burrow#656",
        "https://github.com/FieldmouseWorks/burrow",
        "https://github.com/FieldmouseWorks/burrow/commit/9c6ad02",
        "https://github.com/FieldmouseWorks/burrow/issues/0",
        "http://github.com/FieldmouseWorks/burrow/issues/656",
        "https://example.com/FieldmouseWorks/burrow/issues/656",
        "https://github.com/FieldmouseWorks/burrow/issues/656#issuecomment-1",
        "",
    ] {
        let text = register(&[lesson("a", r#""flow""#, "open")
            .replace("https://github.com/FieldmouseWorks/remi/pull/2", bad)]);
        let e = refused(&text);
        assert!(
            e.contains("isn't a link to a GitHub issue or pull request"),
            "{bad}: {e}"
        );
    }
    let none = lesson("a", r#""flow""#, "open").replace(
        r#"from = ["https://github.com/FieldmouseWorks/burrow/issues/1", "https://github.com/FieldmouseWorks/remi/pull/2"]"#,
        "from = []",
    );
    assert!(refused(&register(&[none])).contains("from names no source"));
}

#[test]
fn applies_names_only_declared_components() {
    let e = refused(&register(&[lesson("a", r#""flow", "steward""#, "open")]));
    assert!(
        e.contains(r#"applies to "steward", which isn't a declared component"#),
        "{e}"
    );
    // A near miss is still unknown.
    let e = refused(&register(&[lesson("a", r#""Flow""#, "open")]));
    assert!(e.contains("isn't a declared component"), "{e}");
    let e = refused(&register(&[lesson("a", "", "open")]));
    assert!(e.contains("applies names no component"), "{e}");
    let e = refused(&register(&[lesson("a", r#""flow", "flow""#, "open")]));
    assert!(e.contains(r#"applies names "flow" twice"#), "{e}");
}

#[test]
fn ids_are_unique_and_nonblank() {
    let e = refused(&register(&[
        lesson("a", r#""flow""#, "open"),
        lesson("b", r#""flow""#, "open"),
        lesson("a", r#""flow""#, "built"),
    ]));
    assert!(e.contains(r#"lesson "a": the id is used twice"#), "{e}");
    for bad in ["", " ", "Has Caps", "trailing-", "a_b"] {
        let e = refused(&register(&[lesson(bad, r#""flow""#, "open")]));
        assert!(
            e.contains("its id isn't lowercase words joined by hyphens"),
            "{bad:?}: {e}"
        );
    }
}

#[test]
fn status_is_open_designed_or_built() {
    for bad in ["done", "Open", "", "closed"] {
        let e = refused(&register(&[lesson("a", r#""flow""#, bad)]));
        assert!(e.contains("unknown variant"), "{bad}: {e}");
    }
}

#[test]
fn a_lesson_is_one_line() {
    for bad in [
        r#""""#,
        r#"" leading""#,
        r#""two\nlines""#,
        "'''\nfirst\nsecond'''",
    ] {
        let text = register(&[lesson("a", r#""flow""#, "open")
            .replace(r#"lesson = "One line.""#, &format!("lesson = {bad}"))]);
        assert!(
            refused(&text).contains("the lesson must be one nonblank line"),
            "{bad}"
        );
    }
}

#[test]
fn where_names_what_carries_it() {
    let text = register(&[lesson("a", r#""flow""#, "open")
        .replace(r#"where = ["docs/DESIGN.md#properties"]"#, "where = []")]);
    assert!(refused(&text).contains("where names nothing that carries it"));
}

#[test]
fn every_component_is_shipped_by_a_declared_milestone() {
    let body = lesson("a", r#""flow""#, "open");
    let with = |components: &str| {
        format!("milestones = [\"M6\", \"M7\"]\n[components]\n{components}\n{body}")
    };
    let e = refused(&with(
        "\"flow\" = [\"M6\"]\n\"x\" = [\"M9\"]\n\"y\" = [\"M7\"]",
    ));
    assert!(
        e.contains(r#"component "x" is shipped by "M9", which isn't a declared milestone"#),
        "{e}"
    );
    let e = refused(&with("\"flow\" = [\"M6\"]\n\"x\" = []\n\"y\" = [\"M7\"]"));
    assert!(
        e.contains(r#"component "x" is shipped by no milestone"#),
        "{e}"
    );
    // A declared milestone that ships nothing would always exit.
    let e = refused(&with("\"flow\" = [\"M6\"]"));
    assert!(e.contains("milestone M7 ships no component"), "{e}");
    // A component is declared once.
    let e = refused(&with("\"flow\" = [\"M6\"]\n\"flow\" = [\"M7\"]"));
    assert!(e.contains("duplicate key"), "{e}");
}

#[test]
fn an_open_lesson_holds_the_exit_of_each_milestone_that_ships_its_component() {
    let r = Register::parse(&register(&[
        lesson("flow-open", r#""flow""#, "open"),
        lesson("daemon-open", r#""conaryd""#, "open"),
        lesson("setup-designed", r#""M7 standalone setup""#, "designed"),
    ]))
    .unwrap();

    let m6 = r.exit("M6").unwrap();
    assert!(!m6.clear());
    let held: Vec<_> = m6
        .open
        .iter()
        .map(|b| (b.id.as_str(), b.applies.clone()))
        .collect();
    assert_eq!(
        held,
        [
            ("flow-open", vec!["flow".to_owned()]),
            ("daemon-open", vec!["conaryd".to_owned()])
        ]
    );

    // flow is M6's alone, so only the lesson on conaryd, which both ship,
    // holds M7.
    let m7 = r.exit("M7").unwrap();
    let held: Vec<_> = m7.open.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(held, ["daemon-open"]);
}

#[test]
fn designed_and_built_lessons_let_a_milestone_exit() {
    let r = Register::parse(&register(&[
        lesson("a", r#""flow", "conaryd""#, "designed"),
        lesson("b", r#""flow""#, "built"),
        lesson("c", r#""M7 standalone setup""#, "built"),
    ]))
    .unwrap();
    for m in ["M6", "M7"] {
        let exit = r.exit(m).unwrap();
        assert!(exit.clear(), "{m}: {exit:?}");
        assert!(!exit.ships.is_empty());
    }
}

#[test]
fn an_open_lesson_on_another_milestones_component_doesnt_hold_this_one() {
    let r = Register::parse(&register(&[
        lesson("setup-open", r#""M7 standalone setup""#, "open"),
        lesson("flow-built", r#""flow""#, "built"),
    ]))
    .unwrap();
    assert!(r.exit("M6").unwrap().clear());
    assert!(!r.exit("M7").unwrap().clear());
}

#[test]
fn a_milestone_the_register_doesnt_declare_is_refused() {
    let r = Register::parse(&register(&[lesson("a", r#""flow""#, "built")])).unwrap();
    let e = r.exit("M8").unwrap_err().to_string();
    assert!(e.contains("declares no milestone M8"), "{e}");
}
