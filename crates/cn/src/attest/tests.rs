use super::*;

// Change logs between claim preparation and an unsigned write. Signed
// requests exercise the same boundary through the token service in the CLI
// integration tests; an unsigned write must retain the snapshot as well.
#[test]
fn unsigned_commands_write_the_snapshot_they_hashed() {
    for kind in ["commit", "revision", "negative"] {
        for change in ["append", "replace", "remove"] {
            let dir = tempfile::tempdir().unwrap();
            let one = dir.path().join("one");
            let two = dir.path().join("two");
            std::fs::write(&one, b"first\n").unwrap();
            std::fs::write(&two, b"second\x00\xff").unwrap();
            let original = b"leg 1 of 2:\nfirst\nleg 2 of 2:\nsecond\x00\xff";
            let out = Out {
                log: None,
                leg_logs: vec![one.clone(), two],
                log_artifact: "logs".to_owned(),
                out: dir.path().join("out"),
                sign: false,
            };
            let gate = GateArgs {
                result: Gate::Pass,
                tree: "7e".repeat(20),
                skipped: vec![],
            };
            let command = match kind {
                "commit" => AttestCommand::Commit {
                    commit: "c0".repeat(20),
                    gate,
                    out,
                },
                "revision" => AttestCommand::Revision {
                    base: "ba".repeat(20),
                    head: "ed".repeat(20),
                    gate,
                    out,
                },
                "negative" => AttestCommand::Negative {
                    base: "ba".repeat(20),
                    head: "ed".repeat(20),
                    result: Some(Negative::Fail),
                    tree: Some("7e".repeat(20)),
                    tests: vec![],
                    checks: vec![],
                    skipped: vec![],
                    unrun: vec![],
                    from: None,
                    out,
                },
                _ => unreachable!(),
            };
            let (claim, out, bytes) = claim(&command).unwrap();
            match change {
                "append" => {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&one)
                        .unwrap()
                        .write_all(b"appended")
                        .unwrap();
                }
                "replace" => {
                    std::fs::remove_file(&one).unwrap();
                    std::fs::write(&one, b"replacement").unwrap();
                }
                "remove" => std::fs::remove_file(&one).unwrap(),
                _ => unreachable!(),
            }
            let made = make(&claim, out, &bytes, None).unwrap();
            let kept = std::fs::read(made.log.unwrap()).unwrap();
            assert_eq!(kept, original, "{kind} {change}");
            let predicate: serde_json::Value =
                serde_json::from_slice(&std::fs::read(made.predicate).unwrap()).unwrap();
            assert_eq!(
                predicate["log"],
                serde_json::to_value(Log::of(&kept, "logs")).unwrap()
            );
            assert!(made.attestation.is_none());
        }
    }
}
