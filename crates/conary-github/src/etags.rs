//! GitHub's answers to the writer's reads, kept beside the ledger with the
//! ETag each came with. A read sends the ETag back, and GitHub answers 304,
//! at no cost to the rate limit, when nothing changed: so a sync that finds
//! nothing new costs almost none of the quota every automation on the box
//! shares. It's only a cache: an entry that can't be read is asked for
//! again, and losing it all costs calls.
//!
//! The answers are of whatever the token can read, private repositories
//! included, so the directory is the writer's alone, though the ledger
//! beside it is everyone's to read.

use std::fs::{self, DirBuilder, File, FileTimes};
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha1::{Digest, Sha1};

const DIR: &str = "etags";

/// An entry no read has used for this long goes: a head's checks and
/// statuses, once its PR has closed, are never asked about again.
const KEEP: Duration = Duration::from_secs(14 * 24 * 3600);

pub struct Etags {
    dir: PathBuf,
}

impl Etags {
    /// The cache in the ledger's directory `ledger`, made if it's missing,
    /// with entries unused for two weeks dropped.
    pub fn open(ledger: &Path) -> io::Result<Self> {
        let dir = ledger.join(DIR);
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let now = SystemTime::now();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > KEEP));
            if stale {
                _ = fs::remove_file(entry.path());
            }
        }
        Ok(Self { dir })
    }

    fn path(&self, url: &str) -> PathBuf {
        let name: String = Sha1::digest(url.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.dir.join(name)
    }

    /// The ETag and body kept for `url`, if any. Reading it counts as using
    /// it, so it stays as long as it's read.
    pub fn get(&self, url: &str) -> Option<(String, Vec<u8>)> {
        let path = self.path(url);
        let text = fs::read(&path).ok()?;
        let at = text.iter().position(|&b| b == b'\n')?;
        let etag = String::from_utf8(text[..at].to_vec()).ok()?;
        let body = text[at + 1..].to_vec();
        if let Ok(f) = File::options().write(true).open(&path) {
            _ = f.set_times(FileTimes::new().set_modified(SystemTime::now()));
        }
        Some((etag, body))
    }

    /// Keeps `body`, as `url` answered with `etag`, in place of what was.
    pub fn put(&self, url: &str, etag: &str, body: &[u8]) -> io::Result<()> {
        if etag.contains('\n') {
            return Ok(());
        }
        let path = self.path(url);
        let tmp = path.with_extension("new");
        let mut f = File::create(&tmp)?;
        f.write_all(etag.as_bytes())?;
        f.write_all(b"\n")?;
        f.write_all(body)?;
        f.sync_all()?;
        fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_each_url_s_answer_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        let etags = Etags::open(dir.path()).unwrap();
        assert_eq!(etags.get("https://x/a"), None);
        etags.put("https://x/a", "W/\"1\"", b"[1]").unwrap();
        etags.put("https://x/b", "W/\"2\"", b"[2]\n[3]").unwrap();
        assert_eq!(
            etags.get("https://x/a"),
            Some(("W/\"1\"".into(), b"[1]".to_vec()))
        );
        assert_eq!(
            etags.get("https://x/b"),
            Some(("W/\"2\"".into(), b"[2]\n[3]".to_vec()))
        );
        etags.put("https://x/a", "W/\"4\"", b"[4]").unwrap();
        assert_eq!(
            etags.get("https://x/a"),
            Some(("W/\"4\"".into(), b"[4]".to_vec()))
        );
    }

    #[test]
    fn is_the_writer_s_alone() {
        let dir = tempfile::tempdir().unwrap();
        Etags::open(dir.path()).unwrap();
        let mode = fs::metadata(dir.path().join(DIR)).unwrap().permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o700
        );
    }

    #[test]
    fn drops_what_no_read_has_used_in_two_weeks() {
        let dir = tempfile::tempdir().unwrap();
        let etags = Etags::open(dir.path()).unwrap();
        etags.put("https://x/old", "\"1\"", b"1").unwrap();
        etags.put("https://x/new", "\"2\"", b"2").unwrap();
        let old = SystemTime::now() - KEEP - Duration::from_secs(60);
        File::options()
            .write(true)
            .open(etags.path("https://x/old"))
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();
        let etags = Etags::open(dir.path()).unwrap();
        assert_eq!(etags.get("https://x/old"), None);
        assert!(etags.get("https://x/new").is_some());
    }
}
