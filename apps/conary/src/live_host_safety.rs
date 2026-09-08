// apps/conary/src/live_host_safety.rs

use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationIntent {
    Missing,
    Apply,
}

impl MutationIntent {
    pub fn from_apply_intent(apply_intent: bool) -> Self {
        if apply_intent {
            Self::Apply
        } else {
            Self::Missing
        }
    }

    pub fn is_present(self) -> bool {
        !matches!(self, Self::Missing)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveMutationClass {
    AlwaysLive,
    LiveConaryState,
    SelectedRootState,
    CurrentlyLiveEvenWithRootArguments,
}

pub struct LiveMutationRequest {
    pub command_label: Cow<'static, str>,
    pub class: LiveMutationClass,
    pub dry_run: bool,
    pub intent: MutationIntent,
}

/// A refused apply request retains its command and exact mutation class.
/// Presentation belongs to `ui::diagnostics`; callers can inspect this error
/// through an anyhow context chain without parsing its display text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("command '{command_label}' requires explicit apply intent")]
pub struct LiveMutationRefusal {
    pub command_label: Cow<'static, str>,
    pub class: LiveMutationClass,
}

pub fn require_mutation_intent(request: &LiveMutationRequest) -> anyhow::Result<()> {
    if request.dry_run || request.intent.is_present() {
        return Ok(());
    }
    Err(LiveMutationRefusal {
        command_label: request.command_label.clone(),
        class: request.class,
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_preserves_dry_run_and_apply_intent_gates() {
        for class in [
            LiveMutationClass::LiveConaryState,
            LiveMutationClass::SelectedRootState,
            LiveMutationClass::CurrentlyLiveEvenWithRootArguments,
            LiveMutationClass::AlwaysLive,
        ] {
            for dry_run in [false, true] {
                for intent in [MutationIntent::Missing, MutationIntent::Apply] {
                    let request = LiveMutationRequest {
                        command_label: Cow::Borrowed("conary install"),
                        class,
                        dry_run,
                        intent,
                    };
                    let result = require_mutation_intent(&request);
                    if dry_run || intent == MutationIntent::Apply {
                        assert!(result.is_ok());
                    } else {
                        let error = result.unwrap_err().context("outer command context");
                        let refusal = error.downcast_ref::<LiveMutationRefusal>().unwrap();
                        assert_eq!(refusal.command_label, request.command_label);
                        assert_eq!(refusal.class, class);
                    }
                }
            }
        }
    }
}
