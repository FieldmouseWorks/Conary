//! Machine choices and the judge's session boundary are separate facts.
use super::{Failure, Identity, Offers, Response, Task};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// Supplied by the caller's configuration, never selected by Conary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub engine: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Context {
    #[default]
    Fresh,
    Continue {
        session: String,
    },
    Fork {
        session: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub choice: Option<Choice>,
    pub context: Context,
}

/// Actual execution, reported by the trusted plugin alongside `Response::by`.
/// None means a program, not an unknown model; model plugins name their model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Executed {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context: Context,
}

/// Retained in a verdict's authenticated issuance and producer receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub by: Identity,
    pub execution: Executed,
}

impl Provenance {
    pub fn valid(&self) -> bool {
        !self.by.engine.trim().is_empty()
            && self
                .by
                .session
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty())
            && self
                .execution
                .model
                .as_ref()
                .is_none_or(|m| !m.trim().is_empty())
            && self
                .execution
                .effort
                .as_ref()
                .is_none_or(|e| !e.trim().is_empty())
            && !matches!(self.execution.context, Context::Fork { .. })
            && match &self.execution.context {
                Context::Continue { session } => self.by.session.as_ref() == Some(session),
                _ => true,
            }
            && (self.execution.effort.is_none() || self.execution.model.is_some())
    }
}

#[derive(Default)]
pub(super) struct Sessions(Mutex<History>);

#[derive(Default)]
struct History {
    judges: BTreeMap<String, String>,
    excluded: BTreeSet<String>,
    unidentified_nonjudge: bool,
}

impl History {
    fn used_by_nonjudge(&mut self, session: &str) {
        if session.trim().is_empty() {
            self.unidentified_nonjudge = true;
        } else {
            self.judges.remove(session);
            self.excluded.insert(session.into());
        }
    }
}

fn fail(why: &str) -> Failure {
    Failure(why.into())
}

impl Sessions {
    /// A new independent handle keeps known exclusions, never continuation
    /// eligibility or the unknown context of the preceding handle.
    pub fn independent(&self) -> Result<Self, Failure> {
        let history = self
            .0
            .lock()
            .map_err(|_| fail("session history poisoned"))?;
        let mut excluded = history.excluded.clone();
        excluded.extend(history.judges.keys().cloned());
        Ok(Self(Mutex::new(History {
            excluded,
            ..History::default()
        })))
    }

    /// Only this handle's validated judge result can establish a continuation.
    /// A matching session string supplied by a writer/reviewer is insufficient.
    pub fn prepare(&self, task: &Task, context: &Context) -> Result<(), Failure> {
        let mut history = self
            .0
            .lock()
            .map_err(|_| fail("session history poisoned"))?;
        if !matches!(task, Task::Judge { .. }) {
            if let Context::Continue { session } | Context::Fork { session } = context {
                if history.judges.contains_key(session) {
                    return Err(fail("nonjudge work cannot reuse a judge session"));
                }
                // Record before launch: failed work still used its input context.
                history.used_by_nonjudge(session);
            }
            return Ok(());
        }
        if history.unidentified_nonjudge {
            return Err(fail(
                "judge cannot use a handle after unidentified nonjudge work",
            ));
        }
        match context {
            Context::Fresh => Ok(()),
            Context::Continue { session } => {
                if history.judges.get(session) != Some(&scope(task)?) {
                    return Err(fail("judge cannot continue a foreign session or scope"));
                }
                Ok(())
            }
            Context::Fork { .. } => Err(fail("judge cannot fork a session")),
        }
    }

    /// Observe before validating the answer. A rejected result does not undo
    /// the work or make its reported context safe for later judging.
    pub fn observe(&self, task: &Task, response: &Response) -> Result<(), Failure> {
        if !matches!(task, Task::Judge { .. }) {
            let mut history = self
                .0
                .lock()
                .map_err(|_| fail("session history poisoned"))?;
            if let Some(session) = &response.by.session {
                history.used_by_nonjudge(session);
            } else {
                history.unidentified_nonjudge = true;
            }
            if let Some(execution) = &response.execution
                && let Context::Continue { session } | Context::Fork { session } =
                    &execution.context
            {
                history.used_by_nonjudge(session);
            }
        }
        Ok(())
    }

    /// No usable response means the handle cannot identify the nonjudge
    /// context. Fail closed for judging; a new handle must start fresh.
    pub fn failed(&self, task: &Task) -> Result<(), Failure> {
        if !matches!(task, Task::Judge { .. }) {
            self.0
                .lock()
                .map_err(|_| fail("session history poisoned"))?
                .unidentified_nonjudge = true;
        }
        Ok(())
    }

    pub fn accept(&self, task: &Task, context: &Context, by: &Identity) -> Result<(), Failure> {
        let Task::Judge {
            author, reviewers, ..
        } = task
        else {
            return Ok(());
        };
        let session = by
            .session
            .as_deref()
            .ok_or_else(|| fail("judge needs a session"))?;
        if std::iter::once(author)
            .chain(reviewers)
            .any(|other| other.session.as_deref() == Some(session))
        {
            return Err(fail("judge reused a writer or reviewer session"));
        }
        let mut history = self
            .0
            .lock()
            .map_err(|_| fail("session history poisoned"))?;
        if history.excluded.contains(session) {
            return Err(fail("judge reused an observed session"));
        }
        match context {
            Context::Fresh if history.judges.contains_key(session) => {
                return Err(fail("initial judge reused an existing session"));
            }
            Context::Continue { session: own } if own != session => {
                return Err(fail("judge changed its continuation session"));
            }
            _ => {}
        }
        history.judges.insert(session.into(), scope(task)?);
        Ok(())
    }
}

fn scope(task: &Task) -> Result<String, Failure> {
    serde_json::to_string(task).map_err(|e| Failure(e.to_string()))
}

impl Execution {
    pub(super) fn validate(&self, engine: &str, offers: &Offers) -> Result<(), Failure> {
        if let Some(choice) = &self.choice
            && (choice.engine != engine
                || !offers.allows(choice.model.as_deref(), choice.effort.as_deref()))
        {
            return Err(fail("execution choice is not offered by this plugin"));
        }
        Ok(())
    }

    pub(super) fn verify(&self, actual: &Executed, offers: &Offers) -> Result<(), Failure> {
        if actual.context != self.context
            || !offers.allows(actual.model.as_deref(), actual.effort.as_deref())
            || (!offers.models.is_empty() && actual.model.is_none())
            || self.choice.as_ref().is_some_and(|c| {
                c.model
                    .as_ref()
                    .is_some_and(|m| Some(m) != actual.model.as_ref())
                    || c.effort
                        .as_ref()
                        .is_some_and(|e| Some(e) != actual.effort.as_ref())
            })
        {
            return Err(fail(
                "actual execution differs from the requested execution or offers",
            ));
        }
        Ok(())
    }
}
