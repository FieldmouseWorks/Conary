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
    pub memory: Memory,
    pub decision: Option<Next>,
    pub jobs: Vec<super::jobs::Work>,
    pub failed: Option<Failure>,
    pub failed_review: Option<FailedPost>,
    pub succeeded: std::collections::BTreeSet<Failure>,
    pub sequence: u64,
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
    pub fn path(&self, pr: u64) -> PathBuf {
        self.dir.join(format!("{pr}.json"))
    }
    pub fn load(&self, pr: u64) -> Result<Record> {
        load(&self.path(pr))
    }
    pub fn save(&self, pr: u64, record: &Record) -> Result<()> {
        save(&self.path(pr), record)
    }
    pub fn job_path(&self, pr: u64, sequence: u64) -> PathBuf {
        self.dir.join(format!("{pr}-{sequence}.job"))
    }
    pub fn read(root: &Path, repo: &conary_core::op::Repo, pr: u64) -> Result<Record> {
        load(
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
