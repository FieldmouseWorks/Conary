//! Compare an earlier reviewed change merged onto today's fork with the
//! actual head. Only GETs reach GitHub. Concurrent text edits are merged
//! by git merge-file in private temporary files; no branch or object is written.
use super::*;
use base64::Engine;
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct Entry {
    path: String,
    mode: String,
    sha: String,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct Tree {
    truncated: bool,
    tree: Vec<Entry>,
}
#[derive(Deserialize)]
struct Blob {
    encoding: String,
    content: String,
    sha: String,
}
impl Api {
    fn entries(&self, repo: &str, commit: &str) -> Result<BTreeMap<String, Entry>, ApiError> {
        let tree: Tree = self.get(&format!("/repos/{repo}/git/trees/{commit}?recursive=1"))?;
        if tree.truncated {
            return Err(ApiError::new(
                "tree was truncated while checking carried reviews",
            ));
        }
        let mut result = BTreeMap::new();
        for entry in tree.tree.into_iter().filter(|e| e.kind != "tree") {
            if result.insert(entry.path.clone(), entry).is_some() {
                return Err(ApiError::new("tree repeated a path"));
            }
        }
        Ok(result)
    }
    fn blob(&self, repo: &str, sha: &str) -> Result<Vec<u8>, ApiError> {
        let blob: Blob = self.get(&format!("/repos/{repo}/git/blobs/{sha}"))?;
        if blob.encoding != "base64" || blob.sha != sha {
            return Err(ApiError::new("blob did not match its requested identity"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(blob.content.split_whitespace().collect::<String>())
            .map_err(|_| ApiError::new("GitHub returned an invalid blob"))?;
        if blob_id(&bytes) != sha {
            return Err(ApiError::new("blob content did not match its identity"));
        }
        Ok(bytes)
    }
    pub(super) fn carries(
        &self,
        repo: &str,
        base: &str,
        head: &str,
        old: &str,
    ) -> Result<bool, ApiError> {
        let fork = self.merge_base(repo, base, head)?;
        let left = self.merge_base(repo, base, old)?;
        let ancestor = self.entries(repo, &left)?;
        let onto = self.entries(repo, &fork)?;
        let reviewed = self.entries(repo, old)?;
        let actual = self.entries(repo, head)?;
        let paths: BTreeSet<_> = ancestor
            .keys()
            .chain(onto.keys())
            .chain(reviewed.keys())
            .chain(actual.keys())
            .collect();
        for path in paths {
            let (a, b, c, want) = (
                ancestor.get(path),
                onto.get(path),
                reviewed.get(path),
                actual.get(path),
            );
            let simple = if a == b {
                Some(c)
            } else if a == c || b == c {
                Some(b)
            } else {
                None
            };
            if let Some(expected) = simple {
                if expected != want {
                    return Ok(false);
                }
                continue;
            }
            let (Some(a), Some(b), Some(c), Some(want)) = (a, b, c, want) else {
                return Ok(false);
            };
            if ![a, b, c, want]
                .iter()
                .all(|e| e.kind == "blob" && ["100644", "100755"].contains(&e.mode.as_str()))
            {
                return Ok(false);
            }
            let mode = if a.mode == b.mode {
                &c.mode
            } else if a.mode == c.mode || b.mode == c.mode {
                &b.mode
            } else {
                return Ok(false);
            };
            if &want.mode != mode {
                return Ok(false);
            }
            let (a, b, c) = (
                self.blob(repo, &a.sha)?,
                self.blob(repo, &b.sha)?,
                self.blob(repo, &c.sha)?,
            );
            let Some(merged) = merge(&a, &b, &c)? else {
                return Ok(false);
            };
            if blob_id(&merged) != want.sha {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
fn blob_id(bytes: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()).as_bytes());
    hash.update(bytes);
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
fn merge(ancestor: &[u8], onto: &[u8], reviewed: &[u8]) -> Result<Option<Vec<u8>>, ApiError> {
    let fail = |_: std::io::Error| ApiError::new("could not check carried text edits");
    let temp = tempfile::tempdir().map_err(fail)?;
    for (name, bytes) in [
        ("ancestor", ancestor),
        ("onto", onto),
        ("reviewed", reviewed),
    ] {
        std::fs::write(temp.path().join(name), bytes).map_err(fail)?;
    }
    let out = std::process::Command::new("git")
        .args(["merge-file", "--stdout", "--diff3", "--"])
        .args(["onto", "ancestor", "reviewed"])
        .current_dir(temp.path())
        .output()
        .map_err(fail)?;
    if out.status.success() {
        Ok(Some(out.stdout))
    } else if out.status.code().is_some_and(|c| (1..=127).contains(&c)) {
        Ok(None)
    } else {
        Err(ApiError::new("git could not compare carried text edits"))
    }
}
