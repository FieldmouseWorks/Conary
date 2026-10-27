//! Merge bases GitHub worked out, kept beside the ledger by its writer. The
//! merge base of two commits never changes, so each pair is asked about
//! once: that's what lets sync check every recently closed PR's whole
//! revision on every run without asking GitHub again. It's only a cache, so
//! a line it can't read is skipped, and losing it costs API calls.

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
        let known = text
            .lines()
            .filter_map(|l| {
                let [repo, a, b, base] = l.split(' ').collect::<Vec<_>>()[..] else {
                    return None;
                };
                Some(((repo.into(), a.into(), b.into()), base.into()))
            })
            .collect();
        Ok(Self { path, known })
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
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(format!("{repo} {a} {b} {base}\n").as_bytes()))
            .map_err(BasesError::Io)?;
        self.known.insert(key, base.clone());
        Ok(base)
    }
}
