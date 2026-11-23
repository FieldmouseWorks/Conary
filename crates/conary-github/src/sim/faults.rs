//! GitHub misbehaving on cue: what a test can make any call do.

use std::cell::RefCell;

use crate::api::{ApiError, PER_PAGE, Page, READ_FOR, paged};
use crate::train::Mark;

use super::{Hub, World};

/// Each kind of call the simulated GitHub answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Call {
    Pull,
    FlowSnapshot,
    CheckSuites,
    FlowStatus,
    FlowStage,
    FlowComment,
    FlowReply,
    FlowResolve,
    FlowUpdate,
    FlowRerun,
    FlowRerequest,
    FlowClose,
    FlowOwner,
    FlowDisable,
    Annotations,
    Pulls,
    CheckRuns,
    Statuses,
    Reviews,
    Comments,
    Commit,
    CompareCommits,
    Pushes,
    CommitPulls,
    Timeline,
    MergeBase,
    Files,
    Diff,
    Job,
    WorkflowRun,
    Artifacts,
    Artifact,
    Keys,
    DefaultBranch,
    Branch,
    Branches,
    CreateBranch,
    DeleteBranch,
    Merge,
    Tree,
    Mark,
    Land,
    MakeCommit,
    OpenPull,
    PullsFrom,
    Ref,
    RefCommit,
    SetRef,
}

/// Which calls a fault is for: a kind, and if a test narrows it, only in
/// one repo, about one PR, or posting one kind of the train's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct On {
    call: Call,
    repo: Option<String>,
    pr: Option<u64>,
    mark: Option<Mark>,
}

impl From<Call> for On {
    fn from(call: Call) -> Self {
        Self {
            call,
            repo: None,
            pr: None,
            mark: None,
        }
    }
}

impl Call {
    pub(crate) fn repo(self, repo: &str) -> On {
        On::from(self).repo(repo)
    }

    pub(crate) fn pr(self, pr: u64) -> On {
        On::from(self).pr(pr)
    }
}

impl On {
    pub(crate) fn repo(mut self, repo: &str) -> Self {
        self.repo = Some(repo.into());
        self
    }

    pub(crate) fn pr(mut self, pr: u64) -> Self {
        self.pr = Some(pr);
        self
    }

    pub(crate) fn mark(mut self, mark: Mark) -> Self {
        self.mark = Some(mark);
        self
    }

    fn covers(&self, asked: &Asked<'_>) -> bool {
        self.call == asked.call
            && self.repo.as_deref().is_none_or(|r| r == asked.repo)
            && self.pr.is_none_or(|n| Some(n) == asked.pr)
            && self.mark.is_none_or(|m| Some(m) == asked.mark)
    }
}

/// One call being answered, as faults see it.
#[derive(Clone)]
pub(super) struct Asked<'a> {
    pub(super) call: Call,
    pub(super) repo: &'a str,
    pub(super) pr: Option<u64>,
    pub(super) mark: Option<Mark>,
}

impl<'a> Asked<'a> {
    pub(super) fn new(call: Call, repo: &'a str) -> Self {
        Self {
            call,
            repo,
            pr: None,
            mark: None,
        }
    }

    pub(super) fn pr(mut self, pr: u64) -> Self {
        self.pr = Some(pr);
        self
    }

    pub(super) fn mark(mut self, mark: Mark) -> Self {
        self.mark = Some(mark);
        self
    }
}

type Race = Box<dyn FnOnce(&Hub)>;
type Watch = Box<dyn Fn(&Hub)>;

#[derive(Default)]
pub(super) struct Faults {
    /// Calls that fail before doing anything, until healed.
    failing: RefCell<Vec<On>>,
    /// Calls GitHub refuses, doing nothing, in the words given, until healed.
    refusing: RefCell<Vec<(On, String)>>,
    /// Writes that are done, and their answer lost, until healed.
    losing: RefCell<Vec<On>>,
    /// Reads that answer from GitHub as it was, until fresh again.
    stale: RefCell<Vec<(On, World)>>,
    /// What happens on GitHub just before a call, once.
    races: RefCell<Vec<(On, Race)>>,
    /// What's run just before every call.
    watch: RefCell<Option<Watch>>,
}

impl Hub {
    /// Calls `on` covers fail, doing nothing, until [`Hub::heal`]ed.
    pub(crate) fn fail(&self, on: impl Into<On>) {
        self.faults.failing.borrow_mut().push(on.into());
    }

    /// Calls `on` covers are refused, doing nothing, GitHub saying `said`,
    /// until [`Hub::heal`]ed.
    pub(crate) fn refuse_with(&self, on: impl Into<On>, said: &str) {
        self.faults
            .refusing
            .borrow_mut()
            .push((on.into(), said.into()));
    }

    /// Writes `on` covers are done, and GitHub's answer is lost, until
    /// [`Hub::heal`]ed.
    pub(crate) fn lose(&self, on: impl Into<On>) {
        self.faults.losing.borrow_mut().push(on.into());
    }

    /// Calls of this kind fail and lose nothing any more.
    pub(crate) fn heal(&self, call: Call) {
        self.faults.failing.borrow_mut().retain(|f| f.call != call);
        self.faults
            .refusing
            .borrow_mut()
            .retain(|(f, _)| f.call != call);
        self.faults.losing.borrow_mut().retain(|f| f.call != call);
    }

    /// Reads `on` covers answer from GitHub as it is now, whatever
    /// changes, until [`Hub::fresh`]: its lag, one endpoint or one PR
    /// behind the rest.
    pub(crate) fn stale(&self, on: impl Into<On>) {
        let now = self.world.borrow().clone();
        self.faults.stale.borrow_mut().push((on.into(), now));
    }

    /// Reads of this kind catch up.
    pub(crate) fn fresh(&self, call: Call) {
        self.faults
            .stale
            .borrow_mut()
            .retain(|(on, _)| on.call != call);
    }

    /// Just before the next call `on` covers, `then` changes GitHub.
    pub(crate) fn race(&self, on: impl Into<On>, then: impl FnOnce(&Hub) + 'static) {
        self.faults
            .races
            .borrow_mut()
            .push((on.into(), Box::new(then)));
    }

    /// Just before every call from now on, `each` sees GitHub and may change
    /// it. A call `each` makes itself isn't watched.
    pub(crate) fn watch(&self, each: impl Fn(&Hub) + 'static) {
        *self.faults.watch.borrow_mut() = Some(Box::new(each));
    }

    /// Runs what watches, logs a call, runs what races it, and fails it if
    /// it's failing.
    fn begin(&self, asked: &Asked<'_>, args: &[&str]) -> Result<(), ApiError> {
        let watch = self.faults.watch.take();
        if let Some(watch) = watch {
            watch(self);
            *self.faults.watch.borrow_mut() = Some(watch);
        }
        self.log
            .borrow_mut()
            .push((asked.call, args.iter().map(|a| (*a).to_owned()).collect()));
        let race = {
            let mut races = self.faults.races.borrow_mut();
            races
                .iter()
                .position(|(on, _)| on.covers(asked))
                .map(|i| races.remove(i).1)
        };
        if let Some(race) = race {
            race(self);
        }
        if self.faults.failing.borrow().iter().any(|f| f.covers(asked)) {
            return Err(ApiError::unavailable(format!(
                "{:?}: Server Error",
                asked.call
            )));
        }
        if let Some((_, said)) = self
            .faults
            .refusing
            .borrow()
            .iter()
            .find(|(f, _)| f.covers(asked))
        {
            return Err(
                ApiError::new(format!("{:?}: http status: 403: {said}", asked.call)).saying(said),
            );
        }
        Ok(())
    }

    /// Answers a read, from GitHub as it is or, if it lags, as it was.
    pub(super) fn read<T>(
        &self,
        asked: Asked<'_>,
        args: &[&str],
        answer: impl FnOnce(&World) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.begin(&asked, args)?;
        let stale = self.faults.stale.borrow();
        if let Some((_, then)) = stale.iter().find(|(on, _)| on.covers(&asked)) {
            return answer(then);
        }
        drop(stale);
        answer(&self.world.borrow())
    }

    /// Answers a list read as GitHub does, a page of [`PER_PAGE`] at a
    /// time, each page its own call, through the pager the API reads
    /// GitHub's pages with. `list` is the whole list, as it is.
    pub(super) fn read_pages<T: serde::de::DeserializeOwned>(
        &self,
        asked: Asked<'_>,
        args: &[&str],
        list: impl Fn(&World) -> Result<Vec<&serde_json::Value>, ApiError>,
    ) -> Result<Vec<T>, ApiError> {
        paged(&format!("{:?}", asked.call), READ_FOR, |n, _| {
            self.read(asked.clone(), args, |w| {
                let all = list(w)?;
                let from = ((n - 1) * PER_PAGE).min(all.len());
                let to = (n * PER_PAGE).min(all.len());
                Ok(Page {
                    items: all[from..to].iter().map(|v| super::parse(v)).collect(),
                    more: to < all.len(),
                })
            })
        })
    }

    /// Makes a write, and loses its answer if it's losing.
    pub(super) fn write<T>(
        &self,
        asked: Asked<'_>,
        args: &[&str],
        make: impl FnOnce() -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.begin(&asked, args)?;
        let made = make()?;
        if self.faults.losing.borrow().iter().any(|f| f.covers(&asked)) {
            return Err(ApiError::unavailable(format!(
                "{:?}: answer lost",
                asked.call
            )));
        }
        Ok(made)
    }
}
