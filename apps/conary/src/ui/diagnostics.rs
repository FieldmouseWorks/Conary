// apps/conary/src/ui/diagnostics.rs
//! Human diagnostics derived from typed failures and publication facts.

mod ccs;
mod package;
mod verification;
pub(crate) use verification::{verification_failure, write_verification_report};

use crate::commands::generation::publication::PublicationOutcome;
use crate::live_host_safety::{LiveMutationClass, LiveMutationRefusal};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Diagnostic {
    message: String,
    facts: Vec<(&'static str, String)>,
    notes: Vec<String>,
}

impl Diagnostic {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            facts: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn fact(mut self, label: &'static str, value: impl Into<String>) -> Self {
        // Paths and package-provided labels are data, never terminal commands
        // or additional diagnostic rows. Keep the underlying error untouched.
        let value = value.into();
        let mut visible = String::with_capacity(value.len());
        for character in value.chars() {
            if character.is_control() {
                visible.extend(character.escape_debug());
            } else {
                visible.push(character);
            }
        }
        self.facts.push((label, visible));
        self
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    fn body(&self) -> String {
        std::iter::once(self.message.clone())
            .chain(
                self.facts
                    .iter()
                    .map(|(label, value)| super::field_line(label, value)),
            )
            .chain(self.notes.iter().map(|note| super::note_line(note)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn plain_body(&self) -> String {
        std::iter::once(self.message.clone())
            .chain(
                self.facts
                    .iter()
                    .map(|(label, value)| format!("  {label}: {value}")),
            )
            .chain(self.notes.iter().map(|note| format!("note: {note}")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn warn(&self) {
        super::warn(&self.body());
    }
}

pub(crate) fn report_error(error: &anyhow::Error) {
    if error.is::<verification::ReportedVerificationFailure>() {
        return;
    }
    super::error(&from_error(error).body());
}

fn from_error(error: &anyhow::Error) -> Diagnostic {
    if let Some(diagnostic) = package::from_error(error) {
        return diagnostic;
    }
    if let Some(diagnostic) = ccs::from_error(error) {
        return diagnostic;
    }
    if let Some(refusal) = error.downcast_ref::<LiveMutationRefusal>() {
        return mutation_refusal(refusal);
    }
    if let Some(error) = error.downcast_ref::<crate::test_hooks::TestHooksError>() {
        return Diagnostic::new(error.to_string());
    }
    if let Some(error) = error.downcast_ref::<conary_core::Error>() {
        return match error {
            conary_core::Error::DatabaseNotFound(path)
                if std::path::Path::new(path)
                    == conary_core::runtime_root::ConaryRuntimeRoot::default().db_path() =>
            {
                Diagnostic::new("Database not initialized.")
                    .note("Run 'sudo conary system init' to set up the system database and all built-in source feeds.")
            }
            conary_core::Error::DatabaseNotFound(path) => {
                Diagnostic::new("Custom database not initialized.")
                    .fact("Database", path)
                    .note("Run 'conary system init --db-path <PATH>' with the same custom path.")
            }
            conary_core::Error::NotFound(detail) => Diagnostic::new(detail),
            conary_core::Error::ConflictError(detail) => Diagnostic::new("Conflict.")
                .fact("Detail", detail),
            conary_core::Error::PathTraversal(detail) => Diagnostic::new("Path safety violation.")
                .fact("Path", detail)
                .note("This may indicate a malicious or corrupt package."),
            other => Diagnostic::new(other.to_string()),
        };
    }
    // Preserve every unclassified cause as a separate fact; do not infer typed
    // remediation by parsing or matching free-form error text.
    let mut diagnostic = Diagnostic::new(error.to_string());
    for cause in error.chain().skip(1) {
        diagnostic = diagnostic.fact("Cause", cause.to_string());
    }
    diagnostic
}

/// Plain fallback for library consumers, including persisted daemon job errors.
/// It uses the same facts and actions as terminal presentation without ANSI.
pub(crate) fn plain_mutation_refusal(refusal: &LiveMutationRefusal) -> String {
    mutation_refusal(refusal).plain_body()
}

fn mutation_refusal(refusal: &LiveMutationRefusal) -> Diagnostic {
    let impact = match refusal.class {
        LiveMutationClass::LiveConaryState => {
            "May update Conary DB or CAS metadata for this machine."
        }
        LiveMutationClass::SelectedRootState => {
            "May update Conary authority and files in the explicitly selected root."
        }
        LiveMutationClass::CurrentlyLiveEvenWithRootArguments => {
            "May change packages, files, scriptlets, ownership, or the live Conary database."
        }
        LiveMutationClass::AlwaysLive => {
            "May change generation state, boot selection, publication debt, or recovery state."
        }
    };
    let mut diagnostic = Diagnostic::new("Confirmation is required before applying changes.")
        .fact("Command", refusal.command_label.as_ref())
        .fact("Impact", impact);
    if refusal.class == LiveMutationClass::CurrentlyLiveEvenWithRootArguments {
        diagnostic = diagnostic.fact("Root", "Current --root or similar arguments are not sufficient isolation for this command yet.");
    }
    diagnostic = diagnostic
        .note("Use --dry-run when available to preview first.")
        .note("Rerun this command with --yes when you intend to apply it.");
    if refusal.class == LiveMutationClass::AlwaysLive {
        diagnostic = diagnostic.note("For generation and recovery operations, verify the concrete generation, boot, or recovery target before applying.");
    }
    diagnostic
}

pub(crate) fn pending_publication(
    changeset_id: i64,
    outcome: &PublicationOutcome,
) -> Option<Diagnostic> {
    if !outcome.needs_publication {
        return None;
    }
    let retry = outcome
        .retry_command
        .as_deref()
        .unwrap_or(crate::commands::generation::publication::DEFAULT_PUBLICATION_RETRY_COMMAND);
    let mut diagnostic =
        Diagnostic::new("Package mutation committed, but generation publication is pending.")
            .fact("Changeset", changeset_id.to_string());
    if let Some(reason) = &outcome.failure_reason {
        diagnostic = diagnostic.fact("Reason", reason);
    }
    Some(diagnostic.note(format!("Run: {retry}")))
}

#[cfg(test)]
mod tests;
