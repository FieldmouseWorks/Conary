// apps/conary/src/ui/diagnostics/package.rs
//! Human rendering of the same package-failure observations returned to agents.

use super::Diagnostic;
use conary_agent_contract::{NativePreflightCause, NativePreflightFailure, NativePreflightProgram};

pub(super) fn from_error(error: &anyhow::Error) -> Option<Diagnostic> {
    let report = crate::commands::package_failure::package_failure_report(error)?;
    let mut diagnostic = Diagnostic::new(match report.requested_updates {
        Some(total) => format!(
            "{} of {total} requested package update(s) failed.",
            report.failures.len()
        ),
        None => "Native transaction preflight refused.".into(),
    });
    for failure in report.failures {
        if let Some(native) = failure.native_preflight {
            if failure.package != native.package || failure.version != native.version {
                diagnostic = diagnostic.fact(
                    "Requested package",
                    format!("{} {}", failure.package, failure.version),
                );
            }
            diagnostic = native_failure(diagnostic, native);
        } else {
            diagnostic = diagnostic
                .fact("Package", failure.package)
                .fact("Version", failure.version);
            for cause in failure.causes {
                diagnostic = diagnostic.fact("Cause", cause);
            }
        }
    }
    if let Some(committed) = report.committed_changesets
        && !committed.is_empty()
    {
        diagnostic = diagnostic.fact("Committed changesets", committed.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
                .note("Earlier committed changes remain applied. Inspect package state and history before retrying.");
    }
    Some(diagnostic)
}

fn native_failure(mut diagnostic: Diagnostic, failure: NativePreflightFailure) -> Diagnostic {
    diagnostic = diagnostic
        .fact("Package", failure.package)
        .fact("Version", failure.version)
        .fact(
            "Architecture",
            failure.architecture.unwrap_or_else(|| "Unspecified".into()),
        )
        .fact("Source format", failure.source_format)
        .fact("Stage", failure.stage)
        .fact(
            "Event",
            if failure.recovery {
                "Recovery"
            } else {
                "Normal"
            },
        );
    if let Some(root) = failure.requested_root {
        diagnostic = diagnostic.fact("Root", root);
    }
    if let Some(database) = failure.database {
        diagnostic = diagnostic.fact("Database", database);
    }
    diagnostic = diagnostic.fact("Execution root", failure.execution_root);
    match failure.program {
        NativePreflightProgram::BundleEntry { entry_id } => {
            diagnostic = diagnostic.fact("Entry", entry_id)
        }
        NativePreflightProgram::Command { argv } => {
            for arg in argv {
                diagnostic = diagnostic.fact("Program argument", arg);
            }
        }
        NativePreflightProgram::RpmSysusers { source_path } => {
            diagnostic = diagnostic.fact("Program", "RPM sysusers");
            if let Some(path) = source_path {
                diagnostic = diagnostic.fact("Source path", path);
            }
        }
    }
    diagnostic = match failure.cause {
        NativePreflightCause::MissingInterpreter {
            interpreter,
            projected,
            ..
        } => diagnostic
            .fact("Cause", "Required interpreter is missing")
            .fact("Interpreter", interpreter)
            .fact(
                "Path state",
                if projected {
                    "Projected at this event"
                } else {
                    "Current selected root"
                },
            ),
        NativePreflightCause::InvalidExecutionRoot { .. } => {
            diagnostic.fact("Cause", "Invalid lifecycle execution root")
        }
        NativePreflightCause::TimeoutOutOfRange {
            timeout_ms,
            minimum_ms,
            maximum_ms,
            ..
        } => diagnostic
            .fact("Cause", "Lifecycle timeout is outside the permitted range")
            .fact("Timeout (ms)", timeout_ms.to_string())
            .fact(
                "Permitted range (ms)",
                format!("{minimum_ms}..={maximum_ms}"),
            ),
        NativePreflightCause::Unclassified { causes } => {
            for cause in causes {
                diagnostic = diagnostic.fact("Cause", cause);
            }
            diagnostic
        }
    };
    for note in failure.notes {
        diagnostic = diagnostic.note(note);
    }
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::scriptlet::NativeLifecyclePreflightError;

    #[test]
    fn typed_native_diagnostic_escapes_facts_and_preserves_scope() {
        let error = crate::commands::package_failure::tests::refusal(
            NativeLifecyclePreflightError::MissingInterpreter {
                interpreter: "/bin/missing\x1b[31m".into(),
                entry_id: "rpm:%pre".into(),
                projected: true,
            },
        );
        let body = from_error(&error).unwrap().plain_body();
        assert!(body.contains("Package: event-owner\\nforged"), "{body}");
        assert!(
            body.contains("Interpreter: /bin/missing\\u{1b}[31m"),
            "{body}"
        );
        assert!(!body.contains('\x1b'));
        assert!(body.contains("Root: /selected' root"));
        assert!(body.contains("Execution root: /tmp/materialized"));
        assert!(body.contains("Path state: Projected at this event"));
        assert_eq!(body.matches("note:").count(), 1);
        assert!(!body.contains("outer context"));
        assert!(!body.contains("committed"));
    }
}
