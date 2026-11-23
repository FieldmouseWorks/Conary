//! Each repo's notes ref, as [`crate::notes`] writes it: a commit and tree
//! on the one before, holding each note's path and blob.

use crate::api::ApiError;
use crate::notes::{Kept, Notes, blob_id};

use super::Hub;
use super::faults::{Asked, Call};

impl Notes for Hub {
    fn get(&self, repo: &str, name: &str) -> Result<Option<Kept>, ApiError> {
        self.read(Asked::new(Call::Notes, repo), &[repo, name], |w| {
            Ok(w.notes.get(&(repo.to_owned(), name.to_owned())).cloned())
        })
    }

    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&Kept>,
        files: &[(String, String)],
    ) -> Result<(), ApiError> {
        self.write(Asked::new(Call::SetNotes, repo), &[repo, name], || {
            let key = (repo.to_owned(), name.to_owned());
            let now = self
                .world
                .borrow()
                .notes
                .get(&key)
                .map(|k| k.commit.clone());
            if now.as_ref() != parent.map(|p| &p.commit) {
                return Err(ApiError::new(format!(
                    "422: {name} moved: Reference cannot be updated"
                )));
            }
            let mut notes = parent.map(|p| p.notes.clone()).unwrap_or_default();
            for (path, text) in files {
                let on: String = path.chars().filter(|&c| c != '/').collect();
                notes.insert(on, (path.clone(), blob_id(text)));
            }
            let kept = Kept {
                commit: self.next_sha(),
                tree: self.next_sha(),
                notes,
            };
            self.world.borrow_mut().notes.insert(key, kept);
            Ok(())
        })
    }
}
