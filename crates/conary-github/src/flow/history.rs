//! Provenance is read from every compared commit. Review coverage is never
//! inferred from a branch name, nor from the author of its final commit.
use super::Settings;
use super::degraded::{Reading, Reads};
use crate::api::History;

pub(super) struct Provenance {
    pub known: bool,
    pub judge: bool,
    pub edited: Option<bool>,
    /// Every engine an `Engine:` trailer names across the commits, `None`
    /// unless every commit read.
    pub engines: Option<std::collections::BTreeSet<String>>,
}

/// The reader uses git's parser, as burrow does. It unfolds continuations
/// and recognizes real trailer blocks rather than searching quoted prose.
pub(super) fn trailers(message: &str) -> Option<Vec<(String, String)>> {
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

/// Provenance from the PR's compared commits, `None` when they didn't read.
/// A comparison that stopped short, or a commit or its trailers that didn't
/// read, is a degraded read in `reads`: what came through it counts as
/// unknown, never as clean.
pub(super) fn provenance(
    got: Option<&History>,
    settings: &Settings,
    renovate: bool,
    reads: &mut Reads,
) -> Provenance {
    let unknown = Provenance {
        known: false,
        judge: false,
        edited: None,
        engines: None,
    };
    let Some(got) = got else {
        return unknown;
    };
    if !got.complete {
        reads.met(
            Reading::Commits,
            "GitHub's comparison stopped short of every commit".into(),
        );
    }
    let mut judge = !got.complete;
    let mut edited = false;
    let mut engines = std::collections::BTreeSet::new();
    for c in &got.commits {
        let Some((detail, message)) = c
            .commit
            .as_ref()
            .and_then(|d| Some((d, d.message.as_deref()?)))
        else {
            reads.met(Reading::Commits, "a commit came without its message".into());
            return unknown;
        };
        let Some(trailers) = trailers(message) else {
            reads.met(
                Reading::Trailers,
                "git couldn't parse a commit's trailers".into(),
            );
            return unknown;
        };
        engines.extend(
            trailers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("engine"))
                .map(|(_, v)| v.clone()),
        );
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
    reads.answered(Reading::Trailers);
    Provenance {
        known: true,
        engines: got.complete.then_some(engines),
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

/// Who wrote the head, as a judge request names it: the one engine its
/// `Engine:` trailers name or, with none, the account that opened the PR,
/// a person or an App. A judge is independent by its fresh session, on any
/// engine (docs/plugins.md, "Independence and execution failures"),
/// so a head no trailer names is judged all the same. Session trailers are
/// author-controlled text, and name no session here: only a completed
/// fix's validated answer does ([`super::observe`]). A history that didn't
/// read in full, or names more than one engine, has no one author: its
/// judge waits, saying why.
pub(super) fn author(
    engines: Option<std::collections::BTreeSet<String>>,
    opener: &str,
) -> Result<conary_core::plugin::Identity, String> {
    let mut engines = engines
        .ok_or("Needs a judge; not every commit read, so its author is unknown")?
        .into_iter();
    let engine = match (engines.next(), engines.next()) {
        (None, _) => opener.to_owned(),
        (Some(one), None) => one,
        (Some(_), Some(_)) => {
            return Err("Needs a judge; its commits name more than one author engine".into());
        }
    };
    Ok(conary_core::plugin::Identity {
        engine,
        session: None,
    })
}
