// apps/conary/src/ui/diagnostics/tests.rs

use super::*;
use std::borrow::Cow;
use std::process::Command;

#[test]
fn wrapped_mutation_refusals_preserve_typed_identity_and_class() {
    for class in [
        LiveMutationClass::LiveConaryState,
        LiveMutationClass::SelectedRootState,
        LiveMutationClass::CurrentlyLiveEvenWithRootArguments,
        LiveMutationClass::AlwaysLive,
    ] {
        let refusal = LiveMutationRefusal {
            command_label: Cow::Borrowed("conary fixture"),
            class,
        };
        let error = anyhow::Error::new(refusal.clone()).context("opaque outer context");
        let diagnostic = from_error(&error);
        assert_eq!(diagnostic, mutation_refusal(&refusal));
        assert_eq!(
            diagnostic.facts[0],
            ("Command", "conary fixture".to_owned())
        );
        assert_eq!(
            diagnostic.notes.len(),
            if class == LiveMutationClass::AlwaysLive {
                3
            } else {
                2
            }
        );
        assert_eq!(
            diagnostic.facts.iter().any(|(label, _)| *label == "Root"),
            class == LiveMutationClass::CurrentlyLiveEvenWithRootArguments
        );
    }
}

#[test]
fn missing_database_notes_preserve_default_and_custom_routes() {
    let default = conary_core::runtime_root::ConaryRuntimeRoot::default()
        .db_path()
        .display()
        .to_string();
    let diagnostic = from_error(&conary_core::Error::DatabaseNotFound(default).into());
    assert_eq!(diagnostic.message, "Database not initialized.");
    assert_eq!(
        diagnostic.notes,
        [
            "Run 'sudo conary system init' to set up the system database and all built-in source feeds."
        ]
    );
    let diagnostic =
        from_error(&conary_core::Error::DatabaseNotFound("/fixture/with spaces.db".into()).into());
    assert_eq!(
        diagnostic.facts,
        [("Database", "/fixture/with spaces.db".to_owned())]
    );
    assert_eq!(
        diagnostic.notes,
        ["Run 'conary system init --db-path <PATH>' with the same custom path."]
    );
}

#[test]
fn unclassified_errors_keep_each_cause_without_inventing_remedies() {
    let error = anyhow::anyhow!("leaf failure")
        .context("inner context")
        .context("outer context");
    let diagnostic = from_error(&error);
    assert_eq!(diagnostic.message, "outer context");
    assert_eq!(
        diagnostic.facts,
        [
            ("Cause", "inner context".to_owned()),
            ("Cause", "leaf failure".to_owned())
        ]
    );
    assert!(diagnostic.notes.is_empty());
    let text_only = anyhow::anyhow!("command 'conary install' requires explicit apply intent");
    assert!(from_error(&text_only).notes.is_empty());
}

#[test]
fn conflicts_stay_domain_neutral_without_inventing_remedies() {
    for detail in [
        "conflicting path",
        "native projection cache logical attestation does not bind its exact catalog",
    ] {
        let diagnostic = from_error(&conary_core::Error::ConflictError(detail.into()).into());
        assert_eq!(diagnostic.message, "Conflict.");
        assert_eq!(diagnostic.facts, [("Detail", detail.to_owned())]);
        assert!(diagnostic.notes.is_empty());
    }
}

#[test]
fn path_safety_keeps_the_affected_path_and_warning() {
    let diagnostic = from_error(&conary_core::Error::PathTraversal("../etc/passwd".into()).into());
    assert_eq!(diagnostic.facts, [("Path", "../etc/passwd".to_owned())]);
    assert_eq!(
        diagnostic.notes,
        ["This may indicate a malicious or corrupt package."]
    );
}

fn pending() -> PublicationOutcome {
    PublicationOutcome {
        generation_number: None,
        state_number: None,
        needs_publication: true,
        retry_command: Some(
            "conary system generation publish --db-path /fixture/conary.db --yes".into(),
        ),
        failure_reason: None,
        completed_debts: 0,
    }
}

#[test]
fn publication_renderer_uses_exact_pending_and_retry_facts() {
    let mut outcome = pending();
    let diagnostic = pending_publication(42, &outcome).unwrap();
    assert_eq!(diagnostic.facts, [("Changeset", "42".to_owned())]);
    assert_eq!(
        diagnostic.notes,
        ["Run: conary system generation publish --db-path /fixture/conary.db --yes"]
    );
    outcome.retry_command = None;
    assert_eq!(
        pending_publication(42, &outcome).unwrap().notes,
        ["Run: conary system generation publish --yes"]
    );
    outcome.failure_reason = Some("typed publication failure".into());
    assert_eq!(
        pending_publication(42, &outcome).unwrap().facts[1],
        ("Reason", "typed publication failure".to_owned())
    );
    outcome.needs_publication = false;
    assert!(pending_publication(42, &outcome).is_none());
}

#[test]
fn publication_capture_child() {
    if std::env::var_os("CONARY_PUBLICATION_DIAGNOSTIC_CAPTURE").is_none() {
        return;
    }
    conary_bootstrap::init_cli_tracing("warn");
    crate::commands::generation::publication::warn_if_publication_pending(42, &pending());
}

#[test]
fn default_publication_output_has_one_warning_and_one_retry() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    crate::test_hooks::clear_inherited_hooks(&mut command);
    let output = command
        .args([
            "--exact",
            "ui::diagnostics::tests::publication_capture_child",
            "--nocapture",
        ])
        .env("CONARY_PUBLICATION_DIAGNOSTIC_CAPTURE", "1")
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        concat!(
            "warning: Package mutation committed, but generation publication is pending.\n",
            "  Changeset: 42\n",
            "note: Run: conary system generation publish --db-path /fixture/conary.db --yes\n",
        )
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("Package mutation committed"), "{stdout}");
    assert!(
        !stdout.contains("Retained generation publication state"),
        "{stdout}"
    );
}

#[test]
fn refusal_display_keeps_plain_guidance_for_library_consumers() {
    let refusal = LiveMutationRefusal {
        command_label: Cow::Borrowed("conaryd install"),
        class: LiveMutationClass::CurrentlyLiveEvenWithRootArguments,
    };
    let message = refusal.to_string();
    assert_eq!(message, mutation_refusal(&refusal).plain_body());
    assert!(message.contains("--dry-run"));
    assert!(message.contains("--yes"));
    assert!(!message.contains('\x1b'));
}
