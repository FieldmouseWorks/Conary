//! Standalone contract tests against `conary-plugin` with no `conary-core`
//! or `conary-github` dependency.
use conary_plugin::*;
use serde_json::json;

#[test]
fn crate_major_version_matches_published_wire_contract() {
    let major: u8 = env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap();
    assert_eq!(Some(major), version(CONTRACT));
    assert_eq!(CONTRACTS.len(), usize::from(major));
    for (index, contract) in CONTRACTS.iter().enumerate() {
        assert_eq!(version(contract), Some((index + 1) as u8));
    }
    assert_eq!(version("conary-plugin v0"), None);
    assert_eq!(version("conary-plugin v99"), None);
}

#[test]
fn path_oid_digest_and_identity_validation() {
    for valid in ["src/lib.rs", "./a/b.rs", "a/b/../c.rs"] {
        assert!(inside_repository(valid), "{valid}");
    }
    for invalid in [
        "",
        " ",
        ".",
        "..",
        "/etc/passwd",
        "../escape.rs",
        "a/../../escape.rs",
        "a\\b.rs",
        "a/\0b.rs",
    ] {
        assert!(!inside_repository(invalid), "{invalid}");
    }

    assert!(valid_git_oid(&"a".repeat(40)));
    assert!(valid_git_oid(&"A".repeat(40)));
    assert!(valid_git_oid(&"0".repeat(64)));
    assert!(!valid_git_oid(&"a".repeat(39)));
    assert!(!valid_git_oid(&"g".repeat(40)));
    assert!(!valid_git_oid("main"));

    assert!(valid_sha256_digest(&"f".repeat(64)));
    assert!(!valid_sha256_digest(&"F".repeat(64)));
    assert!(!valid_sha256_digest(&"a".repeat(40)));

    assert!(
        valid_identity(&Identity {
            engine: "example".into(),
            session: None,
        })
        .is_ok()
    );
    assert!(
        valid_identity(&Identity {
            engine: " ".into(),
            session: None,
        })
        .is_err()
    );
    assert!(
        valid_identity(&Identity {
            engine: "example".into(),
            session: Some(" ".into()),
        })
        .is_err()
    );
}

#[test]
fn task_validation_enforces_oids_and_required_fields() {
    let oid = "1".repeat(40);
    let digest = "a".repeat(64);
    let author = Identity {
        engine: "author".into(),
        session: Some("s1".into()),
    };

    assert!(
        valid_task(&Task::Review {
            repo: "o/r".into(),
            pr: 1,
            base: oid.clone(),
            head: oid.clone(),
            author: author.clone(),
        })
        .is_ok()
    );
    assert!(
        valid_task(&Task::Review {
            repo: "o/r".into(),
            pr: 1,
            base: "short".into(),
            head: oid.clone(),
            author: author.clone(),
        })
        .is_err()
    );
    assert!(
        valid_task(&Task::Fix {
            repo: "o/r".into(),
            pr: 0,
            head: oid.clone(),
            problems: vec![],
        })
        .is_err()
    );
    assert!(
        valid_task(&Task::Judge {
            repo: "o/r".into(),
            pr: 1,
            head: oid.clone(),
            against: "criteria".into(),
            author,
            reviewers: vec![],
            why: vec![Trigger::Asked],
            context: digest,
        })
        .is_ok()
    );
    assert!(
        valid_task(&Task::Prepare {
            repo: "o/r".into(),
            head: oid.clone(),
            tree: oid,
            checks: vec!["gate".into(), "gate".into()],
        })
        .is_err()
    );
    assert!(valid_task(&Task::Describe).is_err());
}

#[test]
fn request_wire_encoding_respects_contract_versions() {
    let task = Task::Proof {
        repo: "o/r".into(),
        head: "2".repeat(40),
        tree: "1".repeat(40),
        checks: vec!["gate".into()],
    };
    let v3 = serde_json::to_value(Request::work(CONTRACT_V3, task.clone())).unwrap();
    assert!(v3.get("head").is_none());
    assert!(v3.get("negotiated").is_none());

    let v4 = serde_json::to_value(Request::work(CONTRACT_V4, task.clone())).unwrap();
    assert_eq!(v4["head"], "2".repeat(40));
    assert!(v4.get("negotiated").is_none());

    let offers = Offers {
        models: vec![],
        capabilities: Capabilities {
            sandbox: Sandbox::None,
            network: false,
            devices: vec![],
            mcp: false,
        },
        meters: vec!["session".into()],
    };
    let mut v5 = Request::work(CONTRACT, task);
    v5.execution = Some(Execution {
        choice: None,
        context: Context::Fresh,
    });
    v5.negotiated = Some(Negotiated {
        by: Identity {
            engine: "proof-engine".into(),
            session: None,
        },
        offers,
    });
    let encoded = serde_json::to_value(&v5).unwrap();
    assert!(encoded.get("negotiated").is_some());
    let decoded: Request = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.contract, CONTRACT);
    assert!(decoded.negotiated.is_some());
    assert!(decoded.execution.is_some());

    // v5 work without negotiated fails decoding.
    let missing_negotiated = json!({
        "contract": CONTRACT,
        "execution": {"choice": null, "context": {"kind": "fresh"}},
        "kind": "proof",
        "repo": "o/r",
        "head": "2".repeat(40),
        "tree": "1".repeat(40),
        "checks": ["gate"],
    });
    assert!(serde_json::from_value::<Request>(missing_negotiated).is_err());
}

#[test]
fn negotiated_verify_and_sessions_accept_work_standalone() {
    let offers = Offers {
        models: vec![Model {
            id: "fixture-model".into(),
            efforts: vec!["quick".into(), "deep".into()],
            context_tokens: Some(128_000),
            max_output_tokens: Some(16_384),
        }],
        capabilities: Capabilities {
            sandbox: Sandbox::Brings,
            network: true,
            devices: vec![],
            mcp: false,
        },
        meters: vec!["session".into()],
    };
    let negotiated = Negotiated {
        by: Identity {
            engine: "judge-engine".into(),
            session: None,
        },
        offers,
    };
    let execution = Execution {
        choice: Some(Choice {
            engine: "judge-engine".into(),
            model: Some("fixture-model".into()),
            effort: Some("deep".into()),
        }),
        context: Context::Fresh,
    };
    let provenance = Provenance {
        by: Identity {
            engine: "judge-engine".into(),
            session: Some("judge-1".into()),
        },
        execution: Executed {
            model: Some("fixture-model".into()),
            effort: Some("deep".into()),
            context: Context::Fresh,
        },
    };
    assert!(provenance.valid());
    negotiated.verify(&execution, &provenance).unwrap();

    let sessions = Sessions::default();
    let judge = Task::Judge {
        repo: "o/r".into(),
        pr: 1,
        head: "2".repeat(40),
        against: "criteria".into(),
        author: Identity {
            engine: "writer".into(),
            session: Some("writer-1".into()),
        },
        reviewers: vec![Identity {
            engine: "reviewer".into(),
            session: Some("reviewer-1".into()),
        }],
        why: vec![Trigger::Asked],
        context: "3".repeat(64),
    };
    sessions
        .accept(&judge, &Context::Fresh, &provenance.by)
        .unwrap();
    for reused in ["writer-1", "reviewer-1"] {
        assert!(
            sessions
                .accept(
                    &judge,
                    &Context::Fresh,
                    &Identity {
                        engine: "judge-engine".into(),
                        session: Some(reused.into()),
                    },
                )
                .is_err()
        );
    }
    // Continuation is allowed only for the same judge session and task.
    sessions
        .accept(
            &judge,
            &Context::Continue {
                session: "judge-1".into(),
            },
            &provenance.by,
        )
        .unwrap();
    assert!(
        sessions
            .accept(
                &judge,
                &Context::Fork {
                    session: "judge-1".into(),
                },
                &provenance.by,
            )
            .is_err()
    );
}

#[test]
fn judgment_words_and_next_steps_validate_strictly() {
    for (judgment, word) in [
        (Judgment::Pass, "pass"),
        (Judgment::Fail, "fail"),
        (Judgment::Owner, "owner"),
        (Judgment::Misdescribed, "misdescribed"),
    ] {
        assert_eq!(judgment.word(), word);
        assert_eq!(Judgment::from_word(word), Some(judgment));
    }
    assert_eq!(Judgment::from_word("land"), None);
    assert_eq!(Judgment::from_word(""), None);

    assert!(Next::Done.valid());
    assert!(
        Next::Await {
            url: "https://example.invalid/pr/1".into()
        }
        .valid()
    );
    assert!(!Next::Await { url: "  ".into() }.valid());
    assert!(
        Next::Ask {
            question: "Which?".into(),
            default: "First".into(),
            one_way: false,
        }
        .valid()
    );
    assert!(
        !Next::Ask {
            question: "Which?".into(),
            default: " ".into(),
            one_way: false,
        }
        .valid()
    );
}

#[test]
fn conformance_fixtures_validate_and_match_expected_answers() {
    let all = fixtures();
    assert!(!all.is_empty());
    for fixture in &all {
        valid_task(&fixture.task).unwrap();
        assert!(version(fixture.contract).is_some());
    }
    assert!(all[0].conforms(&Answer::Review { findings: vec![] }));
    assert!(!all[0].conforms(&Answer::Review {
        findings: vec![Finding {
            path: "src/lib.rs".into(),
            line: 1,
            severity: Severity::P1,
            text: "defect".into(),
        }],
    }));
}

#[cfg(feature = "schemars")]
#[test]
fn schemars_feature_generates_kind_and_choice_schemas() {
    let kind = schemars::schema_for!(Kind).to_value();
    assert!(kind.is_object());
    let choice = schemars::schema_for!(Choice).to_value();
    let properties = choice["properties"].as_object().unwrap();
    for field in ["engine", "model", "effort"] {
        assert_eq!(properties[field]["x-spender"], json!(false), "{field}");
    }
}
