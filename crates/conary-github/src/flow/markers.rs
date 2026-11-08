//! Sealed judge and dependency-sweep verdicts. A quoted marker, even posted
//! by the shared author account, is not the job's attested answer.
use super::{Judged, Judgment, Major, Revision, Settings, fingerprint};
use crate::api::{Comment, PullRequest};
use ring::hmac;
use std::fmt;
use subtle::ConstantTimeEq;

#[derive(Clone, PartialEq, Eq)]
pub struct SealKey(String);
impl fmt::Debug for SealKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealKey([redacted])")
    }
}
impl SealKey {
    pub fn from_hex(text: &str) -> Result<Self, &'static str> {
        if text.len() != 64
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("seal key must be 64 lowercase hexadecimal characters");
        }
        Ok(Self(text.into()))
    }
    /// Same MAC format as burrow's seal.py. The key is the hexadecimal
    /// text, not its decoded bytes; only the first sixteen tag bytes travel.
    pub fn seal(
        &self,
        repo: &str,
        pr: u64,
        name: &str,
        verdict: &str,
        head: &str,
        on: &str,
    ) -> String {
        let k = hmac::Key::new(hmac::HMAC_SHA256, self.0.as_bytes());
        let text = format!("{repo}#{pr} {name} {verdict} {head} {on}");
        hmac::sign(&k, text.as_bytes()).as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    /// Issue a v2 decision. The trusted producer must allocate a strictly
    /// increasing generation per repo/PR/decision kind and persist it before
    /// publishing. Comment order, edits and copied tokens carry no authority.
    pub fn decision(
        &self,
        repo: &str,
        pr: u64,
        name: &str,
        verdict: &str,
        revision: &Revision,
        generation: u64,
    ) -> String {
        let issued = Issued {
            generation,
            revision: revision.clone(),
            verdict: verdict.into(),
        };
        let tag = self.issued_tag(repo, pr, name, &issued);
        let signed = Signed { issued, tag };
        format!(
            "<!-- {name}:v2 {} -->",
            serde_json::to_string(&signed).expect("serializable verdict")
        )
    }
    fn issued_tag(&self, repo: &str, pr: u64, name: &str, issued: &Issued) -> String {
        self.seal(
            &repo.to_lowercase(),
            pr,
            &format!("{name}:v2"),
            &issued.verdict,
            &issued.revision.head,
            &serde_json::to_string(issued).expect("serializable verdict"),
        )
    }
}

pub(super) struct Markers {
    pub judgment: Option<Judged>,
    pub judgment_generation: Option<u64>,
    pub major_generation: Option<u64>,
    pub judge_asked: bool,
    pub major: Option<Major>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Issued {
    generation: u64,
    revision: Revision,
    verdict: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    issued: Issued,
    tag: String,
}

pub(super) fn read(
    repo: &str,
    pull: &PullRequest,
    comments: &[Comment],
    settings: &Settings,
) -> Markers {
    use std::collections::{BTreeMap, BTreeSet};
    let mut result = Markers {
        judgment: None,
        judgment_generation: None,
        major_generation: None,
        judge_asked: false,
        major: None,
    };
    let current = Revision::from(pull);
    let pat =
        regex::Regex::new(r"(?m)^<!-- (autoland-judgment|major-verdict):v2 (\{[^\n]*\}) -->$")
            .expect("constant regex");
    let legacy =
        regex::Regex::new(r"<!-- autoland-judgment: ([a-z]+) ([0-9a-f]{40}) ([0-9a-f]{32}) -->")
            .expect("constant regex");
    let mut judges = BTreeMap::<u64, BTreeSet<String>>::new();
    let mut majors = BTreeMap::<u64, BTreeSet<String>>::new();
    for c in comments.iter().filter(|c| {
        c.user
            .as_ref()
            .is_some_and(|u| settings.verdict_authors.contains(&u.login))
    }) {
        let body = c.body.as_deref().unwrap_or("");
        // Old seals cannot distinguish a new issuance from a replay. They
        // may establish that a judge was requested, never a landing verdict.
        if let Some(key) = &settings.judge_key {
            for m in legacy.captures_iter(body) {
                if m[2] == pull.head.sha
                    && [
                        pull.base.name.clone(),
                        format!(
                            "{} {}",
                            pull.base.name,
                            fingerprint(pull.body.as_deref().unwrap_or(""))
                        ),
                    ]
                    .iter()
                    .any(|on| {
                        verified(
                            key.seal(repo, pull.number, "autoland-judgment", &m[1], &m[2], on),
                            &m[3],
                        )
                    })
                {
                    result.judge_asked = true;
                }
            }
        }
        for m in pat.captures_iter(body) {
            let name = &m[1];
            let Some(key) = (if name == "major-verdict" {
                settings.major_key.as_ref()
            } else {
                settings.judge_key.as_ref()
            }) else {
                continue;
            };
            let Ok(signed) = serde_json::from_str::<Signed>(&m[2]) else {
                continue;
            };
            let issued = &signed.issued;
            if issued.generation == 0
                || !verified(key.issued_tag(repo, pull.number, name, issued), &signed.tag)
            {
                continue;
            }
            if name == "autoland-judgment"
                && issued.revision.head == current.head
                && issued.revision.base == current.base
            {
                result.judge_asked = true;
            }
            if issued.revision != current {
                continue;
            }
            let (out, valid) = if name == "major-verdict" {
                (
                    &mut majors,
                    matches!(issued.verdict.as_str(), "ok" | "held"),
                )
            } else {
                (
                    &mut judges,
                    matches!(
                        issued.verdict.as_str(),
                        "land" | "owner" | "failed" | "misdescribed"
                    ),
                )
            };
            if valid {
                out.entry(issued.generation)
                    .or_default()
                    .insert(issued.verdict.clone());
            }
        }
    }
    // Duplicates are idempotent; conflicting authenticated words at one
    // generation fail closed, rather than allowing comment order to choose.
    let latest = |map: BTreeMap<u64, BTreeSet<String>>| {
        map.into_iter().next_back().and_then(|(_, words)| {
            (words.len() == 1).then(|| words.into_iter().next().expect("one verdict"))
        })
    };
    result.judgment_generation = judges.last_key_value().map(|(g, _)| *g);
    result.major_generation = majors.last_key_value().map(|(g, _)| *g);
    result.major = latest(majors).and_then(|v| match v.as_str() {
        "ok" => Some(Major::Cleared),
        "held" => Some(Major::Held),
        _ => None,
    });
    result.judgment = latest(judges)
        .and_then(|v| match v.as_str() {
            "land" => Some(Judgment::Land),
            "owner" => Some(Judgment::Owner),
            "failed" => Some(Judgment::Failing),
            "misdescribed" => Some(Judgment::Misdescribed),
            _ => None,
        })
        .map(|verdict| Judged {
            revision: current,
            verdict,
        });
    result
}

fn verified(expected: String, tag: &str) -> bool {
    bool::from(expected.as_bytes().ct_eq(tag.as_bytes()))
}
