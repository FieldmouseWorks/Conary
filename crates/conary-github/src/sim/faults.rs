//! GitHub misbehaving on cue: what a test can make any call do.

use std::cell::RefCell;

use crate::api::ApiError;
use crate::train::Mark;

use super::{Hub, World};

/// Each kind of call the simulated GitHub answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Call {
    Pull,
    FlowSnapshot,
    Pulls,
    CheckRuns,
    Statuses,
    Reviews,
    Commit,
    CommitPulls,
    Timeline,
    MergeBase,
    Files,
    Job,
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

#[derive(Default)]
pub(super) struct Faults {
    /// Calls that fail before doing anything, until healed.
    failing: RefCell<Vec<On>>,
    /// Writes that are done, and their answer lost, until healed.
    losing: RefCell<Vec<On>>,
    /// Reads that answer from GitHub as it was, until fresh again.
    stale: RefCell<Vec<(On, World)>>,
    /// What happens on GitHub just before a call, once.
    races: RefCell<Vec<(On, Race)>>,
}

impl Hub {
    /// Calls `on` covers fail, doing nothing, until [`Hub::heal`]ed.
    pub(crate) fn fail(&self, on: impl Into<On>) {
        self.faults.failing.borrow_mut().push(on.into());
    }

    /// Writes `on` covers are done, and GitHub's answer is lost, until
    /// [`Hub::heal`]ed.
    pub(crate) fn lose(&self, on: impl Into<On>) {
        self.faults.losing.borrow_mut().push(on.into());
    }

    /// Calls of this kind fail and lose nothing any more.
    pub(crate) fn heal(&self, call: Call) {
        self.faults.failing.borrow_mut().retain(|f| f.call != call);
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

    /// Logs a call, runs what races it, and fails it if it's failing.
    fn begin(&self, asked: &Asked<'_>, args: &[&str]) -> Result<(), ApiError> {
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
