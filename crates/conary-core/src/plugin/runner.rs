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
    /// The negotiated contract's number, from 1.
    version: u8,
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
            v4_kinds,
            models,
            capabilities,
            meters,
        } = response.answer
        else {
            return bad("expected describe response");
        };
        // The highest contract both sides speak.
        let Some(contract) = CONTRACTS
            .into_iter()
            .rev()
            .find(|c| versions.iter().any(|v| v == c))
        else {
            return bad("invalid advertised versions or kinds");
        };
        let version = super::version(contract).expect("a known contract");
        let mut kinds = kinds;
        if kinds.contains(&Kind::Job) {
            return bad("the job kind belongs in v2_kinds, which a v1 runner doesn't read");
        }
        if kinds.contains(&Kind::Prepare) || v2_kinds.contains(&Kind::Prepare) {
            return bad("the prepare kind belongs in v4_kinds, which an older runner doesn't read");
        }
        if v2_kinds.iter().any(|k| *k != Kind::Job) || v4_kinds.iter().any(|k| *k != Kind::Prepare)
        {
            return bad("invalid advertised versions or kinds");
        }
        if version < 2 && !v2_kinds.is_empty() {
            return bad("the job kind needs conary-plugin v2");
        }
        if version < 4 && !v4_kinds.is_empty() {
            return bad("the prepare kind needs conary-plugin v4");
        }
        kinds.extend(v2_kinds);
        kinds.extend(v4_kinds);
        if kinds.is_empty()
            || kinds
                .iter()
                .enumerate()
                .any(|(i, k)| kinds[..i].contains(k))
        {
            return bad("invalid advertised versions or kinds");
        }
        let offers = if version >= 2 {
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
        if kinds.contains(&Kind::Prepare) && !kinds.contains(&Kind::Proof) {
            return bad("prepare is for proof plugins: it needs the proof kind");
        }
        Ok(Self {
            path: path.into(),
            limits,
            identity: response.by,
            kinds,
            contract,
            version,
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
            Task::Proof { head, tree, .. } | Task::Prepare { head, tree, .. } => {
                sha(head) && sha(tree)
            }
            Task::Job { .. } => true,
            Task::Describe => false,
        };
        if !full_objects {
            return bad("plugin requests require full Git object IDs");
        }
        valid_request(&task)?;
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
        if self.version >= 3 {
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
            (Task::Fix { problems, .. }, Answer::Fix { result, answers }) => {
                match result {
                    FixResult::Pushed { head } if !sha(head) => {
                        return bad("invalid pushed head");
                    }
                    FixResult::Unable { why } if why.trim().is_empty() => {
                        return bad("missing inability reason");
                    }
                    FixResult::Unchanged if self.version < 4 => {
                        return bad("an unchanged fix needs conary-plugin v4");
                    }
                    _ => {}
                }
                self.valid_answers(problems, result, answers.as_deref())?;
            }
            (Task::Judge { .. }, Answer::Judge { why, .. }) if !why.trim().is_empty() => {}
            (Task::Job { .. }, Answer::Job { summary, next })
                if !summary.trim().is_empty() && next.valid() => {}
            // v4 names exactly the requested head; older contracts name none.
            (Task::Proof { head, .. }, Answer::Proof { head: answered, .. })
                if answered.as_ref() != (self.version >= 4).then_some(head) =>
            {
                return bad(if self.version >= 4 {
                    "a v4 proof answer must name the head it ran on"
                } else {
                    "a proof answer names its head only in conary-plugin v4"
                });
            }
            (Task::Prepare { .. }, Answer::Prepare { why, .. }) if !why.trim().is_empty() => {}
            (Task::Proof { checks, .. }, Answer::Proof { results, .. }) => {
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

    /// A v4 fix answers each finding it was handed, once and in order; an
    /// older one carries no answers. A fix it claims needs a pushed head.
    fn valid_answers(
        &self,
        problems: &[Problem],
        result: &FixResult,
        answers: Option<&[FindingAnswer]>,
    ) -> Result<(), Failure> {
        let Some(answers) = answers else {
            if self.version >= 4 {
                return bad("a v4 fix must answer each finding");
            }
            return Ok(());
        };
        if self.version < 4 {
            return bad("fix answers need conary-plugin v4");
        }
        let findings = problems
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p, Problem::Finding { .. }))
            .map(|(i, _)| i);
        if !findings.eq(answers.iter().map(|a| a.problem))
            || answers.iter().any(|a| a.reply.trim().is_empty())
        {
            return bad("a fix must answer each finding once, in order, with a reply");
        }
        if answers.iter().any(|a| a.fixed) && !matches!(result, FixResult::Pushed { .. }) {
            return bad("a fixed answer needs a pushed head");
        }
        Ok(())
    }

    /// Exercise every advertised kind, including its fixture semantics.
    pub fn check(&self) -> Result<Vec<Kind>, Failure> {
        let mut checked = Vec::new();
        let mut sessions = self.sessions.independent()?;
        for fixture in fixtures() {
            let kind = fixture.task.kind().expect("fixture has a kind");
            let since = super::version(fixture.contract).expect("a known contract");
            if !self.kinds.contains(&kind) || self.version < since {
                continue;
            }
            // Fixtures are separate work contexts. A valid nonjudge result
            // without a session must not retire the next fixture's handle;
            // known IDs still exclude a plugin that reuses them for judging.
            let handle = Self {
                path: self.path.clone(),
                limits: self.limits,
                identity: self.identity.clone(),
                kinds: self.kinds.clone(),
                contract: self.contract,
                version: self.version,
                offers: self.offers.clone(),
                sessions,
                calls: std::sync::Mutex::new(()),
            };
            let response = handle.call(fixture.task.clone())?;
            sessions = handle.sessions.independent()?;
            if !fixture.conforms(&response.answer) {
                return bad("answer does not satisfy conformance fixture");
            }
            if !checked.contains(&kind) {
                checked.push(kind);
            }
        }
        Ok(checked)
    }
}

/// What every request must say, whichever contract carries it. The v4
/// fields are checked even when an older wire leaves them off, so the flow
/// can't build a request a v4 plugin would get wrong.
fn valid_request(task: &Task) -> Result<(), Failure> {
    match task {
        Task::Fix { pr, .. } if *pr == 0 => bad("a fix request needs its PR"),
        Task::Judge {
            pr, why, context, ..
        } => {
            if *pr == 0 {
                return bad("a judge request needs its PR");
            }
            if why.is_empty() || why.iter().enumerate().any(|(i, w)| why[..i].contains(w)) {
                return bad("a judge request needs a nonempty, unique why");
            }
            if !digest(context) {
                return bad("a judge request needs its context digest");
            }
            Ok(())
        }
        Task::Prepare { checks, .. }
            if !super::unique_nonblank(checks.iter().map(String::as_str)) =>
        {
            bad("prepare checks must be unique and nonblank")
        }
        _ => Ok(()),
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
/// A sha256 digest in lowercase hex.
fn digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| Failure(format!("invalid plugin JSON: {e}")))?;
    // A prepare answer is never a verdict: it carries nothing but readiness.
    if matches!(request.task, Task::Prepare { .. })
        && value.as_object().is_some_and(|o| {
            o.keys().any(|k| {
                ![
                    "contract",
                    "by",
                    "execution",
                    "kind",
                    "ready",
                    "slot",
                    "why",
                ]
                .contains(&k.as_str())
            })
        })
    {
        return bad("a prepare answer is never a verdict: it carries only ready, slot and why");
    }
    let response: Response =
        serde_json::from_value(value).map_err(|e| Failure(format!("invalid plugin JSON: {e}")))?;
    if response.contract != contract {
        return bad("wrong plugin contract version");
    }
    Ok(response)
}
