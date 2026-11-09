//! Mutable flow state is private local data, never ledger history.
use super::*;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;
    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        map: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }
    pub fn deserialize<
        'de,
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    >(
        deserializer: D,
    ) -> Result<BTreeMap<K, V>, D::Error> {
        Ok(Vec::<(K, V)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub live: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub cleanup: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub landing: Option<Landing>,
    #[serde(default)]
    pub pending: Vec<Landing>,
    pub memory: Memory,
    pub decision: Option<Next>,
    pub jobs: Vec<super::jobs::Work>,
    pub failed: Option<Failure>,
    pub failed_review: Option<FailedPost>,
    pub succeeded: std::collections::BTreeSet<Failure>,
    pub sequence: u64,
}

impl Record {
    /// Older occupancy snapshots could book cancellation of synthetic IDs.
    /// Only retire that reserved namespace when no real owned work has it.
    fn recover_capacity(&mut self) {
        let before = self.memory.stopping.len();
        self.memory.stopping.retain(|id| {
            !id.starts_with("unreadable-") || self.jobs.iter().any(|w| w.job.id == *id)
        });
        if self.memory.stopping.len() != before && self.memory.stopping.is_empty() {
            self.memory.strikes.remove(&Failure::Stop);
            if self
                .memory
                .retry
                .is_some_and(|(kind, _)| kind == Failure::Stop)
            {
                self.memory.retry = None;
            }
            if self.failed == Some(Failure::Stop) {
                self.failed = None;
            }
        }
    }
}
fn record(path: &Path) -> Result<Record> {
    let mut record: Record = load(path)?;
    record.recover_capacity();
    Ok(record)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Landing {
    #[serde(default)]
    pub(crate) permission: Option<crate::ingest::Permission>,
    pub head: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub revision: Option<Revision>,
    pub base: String,
    pub delegated: bool,
    #[serde(default)]
    pub accepted: bool,
    #[serde(default)]
    pub retry: bool,
}

/// One repository lock covers read/decide/act/save. Dry runs only read files.
pub(crate) struct LocalLock(File);
impl LocalLock {
    pub(crate) fn take(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock()?;
        Ok(Self(file))
    }
}
impl Drop for LocalLock {
    fn drop(&mut self) {
        // Closing alone leaves a lock alive in duplicated/inherited file
        // descriptions. A completed pass or worker releases it explicitly.
        let _ = self.0.unlock();
    }
}
pub struct Data {
    dir: PathBuf,
    _lock: LocalLock,
}
impl Data {
    #[cfg(test)]
    pub(crate) fn duplicate_lock(&self) -> File {
        self._lock.0.try_clone().unwrap()
    }

    pub fn open(root: &Path, repo: &conary_core::op::Repo) -> Result<Self> {
        let dir = root.join("flow").join(repo.to_string().to_lowercase());
        fs::create_dir_all(&dir)?;
        let lock = LocalLock::take(&dir.join("lock"))?;
        Ok(Self { dir, _lock: lock })
    }
    pub(crate) fn records(&self) -> Result<Vec<u64>> {
        let mut numbers = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Some(number) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse().ok())
            {
                numbers.push(number);
            }
        }
        numbers.sort_unstable();
        Ok(numbers)
    }
    pub fn path(&self, pr: u64) -> PathBuf {
        self.dir.join(format!("{pr}.json"))
    }
    pub fn load(&self, pr: u64) -> Result<Record> {
        record(&self.path(pr))
    }
    pub fn save(&self, pr: u64, record: &Record) -> Result<()> {
        save(&self.path(pr), record)
    }
    pub fn job_path(&self, pr: u64, sequence: u64) -> PathBuf {
        self.dir.join(format!("{pr}-{sequence}.job"))
    }
    pub fn read(root: &Path, repo: &conary_core::op::Repo, pr: u64) -> Result<Record> {
        record(
            &root
                .join("flow")
                .join(repo.to_string().to_lowercase())
                .join(format!("{pr}.json")),
        )
    }
}
pub(crate) fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}
fn staged(path: &Path, value: &impl Serialize) -> Result<tempfile::NamedTempFile> {
    let parent = path.parent().ok_or("state path has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    Ok(temp)
}
pub(crate) fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    staged(path, value)?.persist(path)?;
    File::open(path.parent().ok_or("state path has no parent")?)?.sync_all()?;
    Ok(())
}
pub(crate) fn save_once(path: &Path, value: &impl Serialize) -> Result<()> {
    match staged(path, value)?.persist_noclobber(path) {
        Ok(_) => File::open(path.parent().ok_or("state path has no parent")?)?.sync_all()?,
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
