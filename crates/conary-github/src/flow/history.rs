//! Provenance is read from every compared commit. Review coverage is never
//! inferred from a branch name, nor from the author of its final commit.
use super::Settings;
use crate::api::History;

pub(super) struct Provenance {
    pub known: bool,
    pub judge: bool,
    pub edited: Option<bool>,
}

/// The reader uses git's parser, as burrow does. It unfolds continuations
/// and recognizes real trailer blocks rather than searching quoted prose.
fn trailers(message: &str) -> Option<Vec<(String, String)>> {
    use std::io::{Seek, Write};
    let mut input = tempfile::tempfile().ok()?;
    input.write_all(message.as_bytes()).ok()?;
    input.rewind().ok()?;
    let out = std::process::Command::new("git")
        .args(["interpret-trailers", "--parse"])
        // PR repository configuration must not influence trailer parsing.
        .current_dir("/")
        .stdin(input)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .map(|line| {
            line.split_once(':')
                .map(|(key, value)| (key.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

pub(super) fn provenance(got: Option<&History>, settings: &Settings, renovate: bool) -> Provenance {
    let Some(got) = got else {
        return Provenance {
            known: false,
            judge: false,
            edited: None,
        };
    };
    let mut judge = !got.complete;
    let mut edited = false;
    for c in &got.commits {
        let Some(detail) = &c.commit else {
            return Provenance {
                known: false,
                judge: false,
                edited: None,
            };
        };
        let Some(message) = detail.message.as_deref() else {
            return Provenance {
                known: false,
                judge: false,
                edited: None,
            };
        };
        let Some(trailers) = trailers(message) else {
            return Provenance {
                known: false,
                judge: false,
                edited: None,
            };
        };
        judge |= trailers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("engine")
                && settings
                    .judge_engines
                    .iter()
                    .any(|e| v.eq_ignore_ascii_case(e))
        });
        edited |= !detail
            .author
            .as_ref()
            .and_then(|a| a.email.as_ref())
            .is_some_and(|e| bot(settings, e));
        edited |= !detail
            .committer
            .as_ref()
            .and_then(|a| a.email.as_ref())
            .is_some_and(|e| e == "noreply@github.com" || bot(settings, e));
        for (_, value) in trailers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("co-authored-by"))
        {
            let email = value
                .rsplit_once('<')
                .and_then(|(_, e)| e.strip_suffix('>'));
            edited |= !email.is_some_and(|e| bot(settings, e));
        }
    }
    Provenance {
        known: true,
        judge,
        edited: if !got.complete || renovate && settings.renovate_email.is_none() {
            None
        } else {
            Some(edited)
        },
    }
}

fn bot(settings: &Settings, email: &str) -> bool {
    settings.bot_emails.contains(email) || settings.renovate_email.as_deref() == Some(email)
}
