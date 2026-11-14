use super::*;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub deadline: Duration,
    pub response_bytes: usize,
}

#[derive(Debug)]
pub struct Failure(pub String);
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Failure {}
impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
fn bad<T>(why: &str) -> Result<T, Failure> {
    Err(Failure(why.into()))
}

pub struct Plugin {
    path: PathBuf,
    limits: Limits,
    pub identity: Identity,
    kinds: Vec<Kind>,
    contract: &'static str,
    offers: Option<Offers>,
    sessions: execution::Sessions,
    // Session observations must precede any later call's eligibility check,
    // including while a nonjudge call is running and has not reported its ID.
    calls: std::sync::Mutex<()>,
}

impl Plugin {
    /// Bound a negotiated plugin's remaining call time.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.limits.deadline = deadline;
        self
    }

    pub fn describe(path: &Path, limits: Limits) -> Result<Self, Failure> {
        Self::describe_cancellable(path, limits, None)
    }

    /// Negotiate long-running work under the controller's cancellation marker.
    pub fn describe_cancellable(
        path: &Path,
        limits: Limits,
        cancelled: Option<&Path>,
    ) -> Result<Self, Failure> {
        let response = execute(path, limits, &Request::describe(), CONTRACT_V1, cancelled)?;
        valid_identity(&response.by)?;
        let Answer::Describe {
            versions,
            kinds,
            v2_kinds,
            models,
            capabilities,
            meters,
        } = response.answer
        else {
            return bad("expected describe response");
        };
        let speaks = |c: &str| versions.iter().any(|v| v == c);
        let contract = if speaks(CONTRACT) {
            CONTRACT
        } else if speaks(CONTRACT_V2) {
            CONTRACT_V2
        } else if speaks(CONTRACT_V1) {
            CONTRACT_V1
        } else {
            return bad("invalid advertised versions or kinds");
        };
        let mut kinds = kinds;
        if kinds.contains(&Kind::Job) {
            return bad("the job kind belongs in v2_kinds, which a v1 runner doesn't read");
        }
        if v2_kinds.iter().any(|k| *k != Kind::Job) {
            return bad("invalid advertised versions or kinds");
        }
        if contract == CONTRACT_V1 && !v2_kinds.is_empty() {
            return bad("the job kind needs conary-plugin v2");
        }
        kinds.extend(v2_kinds);
        if kinds.is_empty()
            || kinds
                .iter()
                .enumerate()
                .any(|(i, k)| kinds[..i].contains(k))
        {
            return bad("invalid advertised versions or kinds");
        }
        let offers = if contract != CONTRACT_V1 {
            let Some(capabilities) = capabilities else {
                return bad("a v2 plugin must declare its capabilities");
            };
            let offers = Offers {
                models,
                capabilities,
                meters,
            };
            if !offers.valid(&kinds) {
                return bad("invalid declared models, capabilities or meters");
            }
            Some(offers)
        } else {
            None
        };
        Ok(Self {
            path: path.into(),
            limits,
            identity: response.by,
            kinds,
            contract,
            offers,
            sessions: execution::Sessions::default(),
            calls: std::sync::Mutex::new(()),
        })
    }

    /// What the plugin declared, when it speaks v2.
    pub fn offers(&self) -> Option<&Offers> {
        self.offers.as_ref()
    }

    pub fn kinds(&self) -> &[Kind] {
        &self.kinds
    }

    pub fn call(&self, task: Task) -> Result<Response, Failure> {
        self.call_cancellable(task, None)
    }

    /// The controller may cancel long work without changing the v1 wire contract.
    pub fn call_cancellable(
        &self,
        task: Task,
        cancelled: Option<&Path>,
    ) -> Result<Response, Failure> {
        self.call_with(task, None, Context::Fresh, cancelled)
    }

    /// Every initial judge starts fresh. Only this handle's own validated
    /// judgment for the same task can be continued; forks are always refused.
    pub fn call_with(
        &self,
        task: Task,
        choice: Option<Choice>,
        context: Context,
        cancelled: Option<&Path>,
    ) -> Result<Response, Failure> {
        let _call = self
            .calls
            .lock()
            .map_err(|_| Failure("plugin call history poisoned".into()))?;
        let Some(kind) = task.kind() else {
            return bad("use describe to negotiate");
        };
        let full_objects = match &task {
            Task::Review { base, head, .. } => sha(base) && sha(head),
            Task::Fix { head, problems, .. } => {
                sha(head)
                    && problems
                        .iter()
                        .all(|p| !matches!(p,Problem::Conflict{base,..} if !sha(base)))
            }
            Task::Judge { head, .. } => sha(head),
            Task::Proof { tree, .. } => sha(tree),
            Task::Job { .. } => true,
            Task::Describe => false,
        };
        if !full_objects {
            return bad("plugin requests require full Git object IDs");
        }
        if !self.kinds.contains(&kind) {
            return bad("request kind was not described");
        }
        let configured_choice = choice.is_some();
        let choice = if let Task::Job { model, effort, .. } = &task {
            if choice.as_ref().is_some_and(|c| {
                model.as_ref().is_some_and(|m| c.model.as_ref() != Some(m))
                    || effort
                        .as_ref()
                        .is_some_and(|e| c.effort.as_ref() != Some(e))
            }) {
                return bad("job and execution choices disagree");
            }
            choice.or_else(|| {
                (model.is_some() || effort.is_some()).then(|| Choice {
                    engine: self.identity.engine.clone(),
                    model: model.clone(),
                    effort: effort.clone(),
                })
            })
        } else {
            choice
        };
        let mut request = Request::work(self.contract, task.clone());
        if self.contract == CONTRACT {
            let execution = Execution { choice, context };
            execution.validate(
                &self.identity.engine,
                self.offers.as_ref().expect("v3 offers"),
            )?;
            request.execution = Some(execution);
        } else if configured_choice || context != Context::Fresh || kind == Kind::Judge {
            return bad("execution choices and independent judges need conary-plugin v3");
        }
        if let Task::Job {
            repo,
            workdir,
            prompt,
            model,
            effort,
        } = &task
        {
            let Some(offers) = &self.offers else {
                return bad("the job kind needs conary-plugin v2");
            };
            if repo.trim().is_empty()
                || workdir.trim().is_empty()
                || workdir.contains('\0')
                || prompt.trim().is_empty()
            {
                return bad("a job needs a repo, workdir and prompt");
            }
            if !offers.allows(model.as_deref(), effort.as_deref()) {
                return bad("job asks for a model or effort the plugin did not offer");
            }
        }
        if let Task::Fix { problems, .. } = &task {
            for problem in problems {
                match problem {
                    Problem::Finding { finding } if !finding.valid() => {
                        return bad("invalid finding");
                    }
                    _ => {}
                }
            }
        }
        if let Some(execution) = &request.execution {
            self.sessions.prepare(&task, &execution.context)?;
        }
        let response = match execute(&self.path, self.limits, &request, self.contract, cancelled) {
            Ok(response) => {
                if request.execution.is_some() {
                    self.sessions.observe(&task, &response)?;
                }
                response
            }
            Err(error) => {
                if request.execution.is_some() {
                    self.sessions.failed(&task)?;
                }
                return Err(error);
            }
        };
        valid_identity(&response.by)?;
        if response.by.engine != self.identity.engine {
            return bad("plugin changed its engine identity");
        }
        if let Some(execution) = &request.execution {
            let Some(actual) = &response.execution else {
                return bad("plugin did not report actual execution");
            };
            execution.verify(actual, self.offers.as_ref().expect("v3 offers"))?;
        }
        if let Task::Review { author, .. } | Task::Judge { author, .. } = &task {
            valid_identity(author)?;
            // A session remains its author even if it switches engines.
            // Unknown sessions cannot establish independence within an engine.
            if (author.session.is_some() && author.session == response.by.session)
                || (kind == Kind::Review
                    && author.engine == response.by.engine
                    && (author.session.is_none() || response.by.session.is_none()))
            {
                return bad("the head's author cannot grade it");
            }
        }
        match (&task, &response.answer) {
            (Task::Review { .. }, Answer::Review { findings }) => {
                if !findings.iter().all(Finding::valid) {
                    return bad("invalid finding");
                }
            }
            (Task::Fix { .. }, Answer::Fix { result }) => match result {
                FixResult::Pushed { head } if !sha(head) => return bad("invalid pushed head"),
                FixResult::Unable { why } if why.trim().is_empty() => {
                    return bad("missing inability reason");
                }
                _ => {}
            },
            (Task::Judge { .. }, Answer::Judge { why, .. }) if !why.trim().is_empty() => {}
            (Task::Job { .. }, Answer::Job { summary, next })
                if !summary.trim().is_empty() && next.valid() => {}
            (Task::Proof { checks, .. }, Answer::Proof { results }) => {
                if checks.len() != results.len()
                    || checks
                        .iter()
                        .enumerate()
                        .any(|(i, c)| checks[..i].contains(c))
                    || checks.iter().any(|c| {
                        results
                            .iter()
                            .filter(|r| &r.check == c && !r.why.trim().is_empty())
                            .count()
                            != 1
                    })
                {
                    return bad("proof must answer each check exactly once with a reason");
                }
            }
            _ => return bad("response kind does not match request or required reason is empty"),
        }
        if let Some(execution) = &request.execution {
            self.sessions
                .accept(&task, &execution.context, &response.by)?;
        }
        Ok(response)
    }

    /// Exercise every advertised kind, including its fixture semantics.
    pub fn check(&self) -> Result<Vec<Kind>, Failure> {
        let mut checked = Vec::new();
        for task in fixtures() {
            let kind = task.kind().expect("fixture has a kind");
            if !self.kinds.contains(&kind) {
                continue;
            }
            let response = self.call(task)?;
            let conforms = match response.answer {
                Answer::Review { findings } => findings.is_empty(),
                Answer::Fix {
                    result: FixResult::Unable { .. },
                } => true,
                Answer::Judge {
                    verdict: Judgment::Pass,
                    ..
                } => true,
                Answer::Proof { results } => results.iter().all(|r| {
                    matches!(
                        (&*r.check, &r.verdict),
                        ("fixture-pass", Verdict::Pass) | ("fixture-fail", Verdict::Fail)
                    )
                }),
                Answer::Job { next, .. } => next == Next::Done,
                _ => false,
            };
            if !conforms {
                return bad("answer does not satisfy conformance fixture");
            }
            checked.push(kind);
        }
        Ok(checked)
    }
}
fn valid_identity(by: &Identity) -> Result<(), Failure> {
    if by.engine.trim().is_empty() || by.session.as_ref().is_some_and(|s| s.trim().is_empty()) {
        bad("empty plugin identity")
    } else {
        Ok(())
    }
}
fn sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn execute(
    path: &Path,
    limits: Limits,
    request: &Request,
    contract: &str,
    cancelled: Option<&Path>,
) -> Result<Response, Failure> {
    // File-backed stdio avoids deadlocks from a child that never reads stdin,
    // fills stdout before reading, or exits leaving a descendant holding a pipe.
    let mut input = tempfile::tempfile()?;
    serde_json::to_writer(&mut input, request).map_err(|e| Failure(e.to_string()))?;
    input.write_all(b"\n")?;
    input.rewind()?;
    let mut output = tempfile::tempfile()?;
    let start = Instant::now();
    let mut child = Command::new(path)
        .stdin(input)
        .stdout(output.try_clone()?)
        .stderr(Stdio::inherit())
        .spawn()?;
    let status = loop {
        let result = (|| {
            if cancelled.is_some_and(Path::exists) {
                return bad("plugin cancelled");
            }
            if start.elapsed() >= limits.deadline {
                return bad("plugin exceeded deadline");
            }
            if output.metadata()?.len() > limits.response_bytes as u64 {
                return bad("plugin response too large");
            }
            Ok(child.try_wait()?)
        })();
        match result {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    };
    if !status.success() {
        return bad("plugin exited unsuccessfully");
    }
    output.rewind()?;
    let mut bytes = Vec::new();
    output
        .take((limits.response_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.response_bytes {
        return bad("plugin response too large");
    }
    let response: Response =
        serde_json::from_slice(&bytes).map_err(|e| Failure(format!("invalid plugin JSON: {e}")))?;
    if response.contract != contract {
        return bad("wrong plugin contract version");
    }
    Ok(response)
}
