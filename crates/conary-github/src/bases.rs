//! Merge bases GitHub worked out, kept beside the ledger by its writer. The
//! merge base of two commits never changes, so each pair is asked about
//! once: that's what lets sync check every recently closed PR's whole
//! revision on every run without asking GitHub again. It's only a cache, so
//! a line that isn't a whole record (a torn append, say) is skipped and
//! asked about again, and losing it costs API calls.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use conary_core::log::Lock;

use crate::api::{ApiError, Source};

const FILE: &str = "merge-bases";

pub struct MergeBases {
    /// Where its writer keeps it; `None` for one read without the lock.
    path: Option<PathBuf>,
    known: HashMap<Key, String>,
    /// What one read without the lock asked GitHub, for the writer to keep.
    asked: Vec<(Key, String)>,
    /// The file ends partway through a line, so the next record starts on a
    /// line of its own.
    torn: bool,
}

/// A repo and two commits in it.
type Key = (String, String, String);

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn record(line: &str) -> Option<(Key, String)> {
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
        let mut bases = Self::read(lock.dir())?;
        bases.path = Some(lock.dir().join(FILE));
        Ok(bases)
    }

    /// The cache in `dir` as it is now, for one who doesn't hold the
    /// ledger's lock: never written, since a torn record the writer is
    /// appending is skipped, and what it asks GitHub is for the writer to
    /// [`keep`](Self::keep).
    pub fn read(dir: &Path) -> io::Result<Self> {
        let text = match fs::read_to_string(dir.join(FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let known = text.lines().filter_map(record).collect();
        let torn = !text.is_empty() && !text.ends_with('\n');
        Ok(Self {
            path: None,
            known,
            asked: Vec::new(),
            torn,
        })
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
        self.put(&format!("{repo} {a} {b} {base}"))
            .map_err(BasesError::Io)?;
        Ok(base)
    }

    /// Keeps what `read`, read without the lock, asked GitHub.
    pub fn keep(&mut self, read: MergeBases) -> io::Result<()> {
        for ((repo, a, b), base) in read.asked {
            if !self
                .known
                .contains_key(&(repo.clone(), a.clone(), b.clone()))
            {
                self.put(&format!("{repo} {a} {b} {base}"))?;
            }
        }
        Ok(())
    }

    /// Learns the record `line`: appended to the file by the writer, kept
    /// for it otherwise. One this cache couldn't read back isn't kept.
    fn put(&mut self, line: &str) -> io::Result<()> {
        let Some((key, base)) = record(line) else {
            return Ok(());
        };
        match &self.path {
            None => self.asked.push((key.clone(), base.clone())),
            Some(path) => {
                let start = if self.torn { "\n" } else { "" };
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut f| f.write_all(format!("{start}{line}\n").as_bytes()))?;
                self.torn = false;
            }
        }
        self.known.insert(key, base);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Call, Hub};

    const REPO: &str = "FieldmouseWorks/burrow";

    #[test]
    fn a_torn_record_costs_a_call_and_never_a_wrong_answer() {
        let dir = tempfile::tempdir().unwrap();
        let lock = Lock::take(dir.path()).unwrap();
        let (a, b, a2) = ("a".repeat(40), "b".repeat(40), "a".repeat(39) + "1");
        // b forked from where a2 did, after a forked: b and a meet at c,
        // b and a2 at c1.
        let (c, c1) = ("c".repeat(40), "c".repeat(39) + "1");
        let gh = Hub::new(REPO, &c);
        gh.change(&a, &c, "a");
        gh.change(&c1, &c, "c1");
        gh.change(&a2, &c1, "a2");
        gh.change(&b, &c1, "b");
        let mut bases = MergeBases::load(&lock).unwrap();
        assert_eq!(bases.get(&gh, REPO, &a, &b).unwrap(), "c".repeat(40));
        assert_eq!(bases.get(&gh, REPO, &a, &b).unwrap(), "c".repeat(40));
        assert_eq!(gh.count(Call::MergeBase), 1);

        // An append cut short partway through its last commit id.
        let path = dir.path().join(FILE);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&format!("{REPO} {a2} {b} {}", "c".repeat(20)));
        std::fs::write(&path, &text).unwrap();
        let mut bases = MergeBases::load(&lock).unwrap();
        assert_eq!(bases.get(&gh, REPO, &a, &b).unwrap(), "c".repeat(40));
        assert_eq!(gh.count(Call::MergeBase), 1);
        assert_eq!(bases.get(&gh, REPO, &a2, &b).unwrap(), "c".repeat(39) + "1");
        assert_eq!(gh.count(Call::MergeBase), 2);

        // The new record went on a line of its own, so both read back.
        let mut bases = MergeBases::load(&lock).unwrap();
        bases.get(&gh, REPO, &a, &b).unwrap();
        bases.get(&gh, REPO, &a2, &b).unwrap();
        assert_eq!(gh.count(Call::MergeBase), 2);
    }

    #[test]
    fn one_read_without_the_lock_writes_nothing_and_its_writer_keeps_what_it_asked() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let gh = Hub::new(REPO, &c);
        gh.change(&a, &c, "a");
        gh.change(&b, &c, "b");
        let mut read = MergeBases::read(dir.path()).unwrap();
        assert_eq!(read.get(&gh, REPO, &a, &b).unwrap(), c);
        assert_eq!(read.get(&gh, REPO, &a, &b).unwrap(), c);
        assert_eq!(gh.count(Call::MergeBase), 1);
        assert!(!dir.path().join(FILE).exists());

        let lock = Lock::take(dir.path()).unwrap();
        MergeBases::load(&lock).unwrap().keep(read).unwrap();
        let mut bases = MergeBases::load(&lock).unwrap();
        assert_eq!(bases.get(&gh, REPO, &a, &b).unwrap(), c);
        assert_eq!(gh.count(Call::MergeBase), 1);
    }
}
