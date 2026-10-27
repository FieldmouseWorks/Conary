//! Merge bases GitHub worked out, kept beside the ledger by its writer. The
//! merge base of two commits never changes, so each pair is asked about
//! once: that's what lets sync check every recently closed PR's whole
//! revision on every run without asking GitHub again. It's only a cache, so
//! a line that isn't a whole record (a torn append, say) is skipped and
//! asked about again, and losing it costs API calls.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use conary_core::log::Lock;

use crate::api::{ApiError, Source};

const FILE: &str = "merge-bases";

pub struct MergeBases {
    path: PathBuf,
    known: HashMap<(String, String, String), String>,
    /// The file ends partway through a line, so the next record starts on a
    /// line of its own.
    torn: bool,
}

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn record(line: &str) -> Option<((String, String, String), String)> {
    let [repo, a, b, base] = line.split(' ').collect::<Vec<_>>()[..] else {
        return None;
    };
    let named = repo
        .split_once('/')
        .is_some_and(|(o, n)| !o.is_empty() && !n.is_empty());
    (named && [a, b, base].into_iter().all(is_commit))
        .then(|| ((repo.into(), a.into(), b.into()), base.into()))
}

#[derive(Debug)]
pub enum BasesError {
    Api(ApiError),
    Io(io::Error),
}

impl MergeBases {
    /// The cache in `lock`'s directory: only its writer keeps one.
    pub fn load(lock: &Lock) -> io::Result<Self> {
        let path = lock.dir().join(FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let known = text.lines().filter_map(record).collect();
        let torn = !text.is_empty() && !text.ends_with('\n');
        Ok(Self { path, known, torn })
    }

    /// The merge base of `a` and `b` in `repo`, from GitHub the first time.
    pub fn get(
        &mut self,
        source: &dyn Source,
        repo: &str,
        a: &str,
        b: &str,
    ) -> Result<String, BasesError> {
        let key = (repo.to_owned(), a.to_owned(), b.to_owned());
        if let Some(base) = self.known.get(&key) {
            return Ok(base.clone());
        }
        let base = source.merge_base(repo, a, b).map_err(BasesError::Api)?;
        let line = format!("{repo} {a} {b} {base}");
        let Some((key, base)) = record(&line) else {
            // Not a record this cache could read back: nothing to keep.
            return Ok(base);
        };
        let start = if self.torn { "\n" } else { "" };
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(format!("{start}{line}\n").as_bytes()))
            .map_err(BasesError::Io)?;
        self.torn = false;
        self.known.insert(key, base.clone());
        Ok(base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{CheckRun, Commit, PullRequest, Pulls, Review, Status};
    use std::cell::Cell;

    /// GitHub, answering only merge bases, and counting them.
    #[derive(Default)]
    struct Bases(Cell<usize>);

    impl Source for Bases {
        fn pull(&self, _: &str, _: u64) -> Result<PullRequest, ApiError> {
            unimplemented!()
        }
        fn pulls(&self, _: &str, _: Pulls) -> Result<Vec<PullRequest>, ApiError> {
            unimplemented!()
        }
        fn check_runs(&self, _: &str, _: &str) -> Result<Vec<CheckRun>, ApiError> {
            unimplemented!()
        }
        fn statuses(&self, _: &str, _: &str) -> Result<Vec<Status>, ApiError> {
            unimplemented!()
        }
        fn reviews(&self, _: &str, _: u64) -> Result<Vec<Review>, ApiError> {
            unimplemented!()
        }
        fn commit(&self, _: &str, _: &str) -> Result<Commit, ApiError> {
            unimplemented!()
        }
        fn commit_pulls(&self, _: &str, _: &str) -> Result<Vec<u64>, ApiError> {
            unimplemented!()
        }
        fn retargeted(&self, _: &str, _: u64) -> Result<Option<String>, ApiError> {
            unimplemented!()
        }
        fn merge_base(&self, _: &str, a: &str, _: &str) -> Result<String, ApiError> {
            self.0.set(self.0.get() + 1);
            Ok(a.replace('a', "c"))
        }
    }

    const REPO: &str = "FieldmouseWorks/burrow";

    #[test]
    fn a_torn_record_costs_a_call_and_never_a_wrong_answer() {
        let dir = tempfile::tempdir().unwrap();
        let lock = Lock::take(dir.path()).unwrap();
        let (a, b, a2) = ("a".repeat(40), "b".repeat(40), "a".repeat(39) + "1");
        let gh = Bases::default();
        let mut bases = MergeBases::load(&lock).unwrap();
        assert_eq!(bases.get(&gh, REPO, &a, &b).unwrap(), "c".repeat(40));
        assert_eq!(bases.get(&gh, REPO, &a, &b).unwrap(), "c".repeat(40));
        assert_eq!(gh.0.get(), 1);

        // An append cut short partway through its last commit id.
        let path = dir.path().join(FILE);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&format!("{REPO} {a2} {b} {}", "c".repeat(20)));
        std::fs::write(&path, &text).unwrap();
        let mut bases = MergeBases::load(&lock).unwrap();
        assert_eq!(bases.get(&gh, REPO, &a, &b).unwrap(), "c".repeat(40));
        assert_eq!(gh.0.get(), 1);
        assert_eq!(bases.get(&gh, REPO, &a2, &b).unwrap(), "c".repeat(39) + "1");
        assert_eq!(gh.0.get(), 2);

        // The new record went on a line of its own, so both read back.
        let mut bases = MergeBases::load(&lock).unwrap();
        bases.get(&gh, REPO, &a, &b).unwrap();
        bases.get(&gh, REPO, &a2, &b).unwrap();
        assert_eq!(gh.0.get(), 2);
    }
}
