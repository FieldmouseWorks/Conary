//! Sealed judge and dependency-sweep verdicts. A quoted marker, even posted
//! by the shared author account, is not the job's attested answer.
use super::{Basis, Judged, Judgment, Major, Revision, Settings, fingerprint};
use crate::api::{Comment, PullRequest};
use ring::hmac;
use std::fmt;
use subtle::ConstantTimeEq;

/// Deserialized only through [`SealKey::from_hex`], so a malformed key in the
/// flow config is refused when the config loads.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct SealKey(String);
impl fmt::Debug for SealKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealKey([redacted])")
    }
}
impl TryFrom<String> for SealKey {
    type Error = &'static str;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::from_hex(&text)
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
            provenance: None,
            why: None,
            context: None,
        };
        self.issue(repo, pr, name, Version::V2, issued)
    }
    /// A producer's new judgment, as a v3 issuance: it retains actual
    /// execution, not requested policy, and seals the `why` and `context` it
    /// was judged under, which `judgment_receipt_by` binds too. Each
    /// argument is a sealed field.
    #[allow(clippy::too_many_arguments)]
    pub fn judgment_by(
        &self,
        repo: &str,
        pr: u64,
        verdict: Judgment,
        revision: &Revision,
        generation: u64,
        provenance: conary_core::plugin::Provenance,
        basis: Basis,
    ) -> Result<String, &'static str> {
        if !provenance.valid() {
            return Err("invalid judge provenance");
        }
        if !basis.valid() {
            return Err("a judgment needs a nonempty, unique why and its context digest");
        }
        Ok(self.issue(
            repo,
            pr,
            "autoland-judgment",
            Version::V3,
            Issued {
                generation,
                revision: revision.clone(),
                verdict: verdict.word().into(),
                provenance: Some(provenance),
                why: Some(basis.why),
                context: Some(basis.context),
            },
        ))
    }
    fn issue(&self, repo: &str, pr: u64, name: &str, version: Version, issued: Issued) -> String {
        let tag = self.issued_tag(repo, pr, name, version, &issued);
        let signed = Signed { issued, tag };
        format!(
            "<!-- {name}:{} {} -->",
            version.word(),
            serde_json::to_string(&signed).expect("serializable verdict")
        )
    }
    /// The version is inside the sealed text, so an issuance can't be read
    /// as another version than the one it was sealed as.
    fn issued_tag(
        &self,
        repo: &str,
        pr: u64,
        name: &str,
        version: Version,
        issued: &Issued,
    ) -> String {
        self.seal(
            &repo.to_lowercase(),
            pr,
            &format!("{name}:{}", version.word()),
            &issued.verdict,
            &issued.revision.head,
            &serde_json::to_string(issued).expect("serializable verdict"),
        )
    }
}

/// The issued format. v2 is a verdict on a revision, with a judgment's
/// provenance when it has one. v3, judgments only, adds what it was judged
/// on, and requires it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    V2,
    V3,
}
impl Version {
    fn word(self) -> &'static str {
        match self {
            Self::V2 => "v2",
            Self::V3 => "v3",
        }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<conary_core::plugin::Provenance>,
    /// v3 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    why: Option<Vec<super::Trigger>>,
    /// v3 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<String>,
}
impl Issued {
    /// What a judgment says it was judged on: always for v3, never for v2.
    /// Err is an authenticated issuance that breaks its version's format.
    fn basis(&self, version: Version) -> Result<Option<Basis>, ()> {
        match (version, &self.why, &self.context) {
            (Version::V2, None, None) => Ok(None),
            (Version::V3, Some(why), Some(context)) if self.provenance.is_some() => {
                let basis = Basis {
                    why: why.clone(),
                    context: context.clone(),
                };
                if basis.valid() {
                    Ok(Some(basis))
                } else {
                    Err(())
                }
            }
            _ => Err(()),
        }
    }
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
        regex::Regex::new(r"(?m)^<!-- (autoland-judgment|major-verdict):(v2|v3) (\{[^\n]*\}) -->$")
            .expect("constant regex");
    let legacy =
        regex::Regex::new(r"<!-- autoland-judgment: ([a-z]+) ([0-9a-f]{40}) ([0-9a-f]{32}) -->")
            .expect("constant regex");
    // A judgment's words are its issued JSON, or None for an authenticated
    // issuance that breaks its version's format: a tombstone.
    let mut judges = BTreeMap::<u64, BTreeSet<Option<String>>>::new();
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
            let version = if &m[2] == "v3" {
                Version::V3
            } else {
                Version::V2
            };
            let Some(key) = (match (name, version) {
                ("major-verdict", Version::V2) => settings.major_key.as_ref(),
                ("autoland-judgment", _) => settings.judge_key.as_ref(),
                _ => None,
            }) else {
                continue;
            };
            let Ok(signed) = serde_json::from_str::<Signed>(&m[3]) else {
                continue;
            };
            let issued = &signed.issued;
            if issued.generation == 0
                || !verified(
                    key.issued_tag(repo, pull.number, name, version, issued),
                    &signed.tag,
                )
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
            // An authenticated newer generation supersedes older words even
            // when its verdict or format is unsupported: decoding it then
            // fails closed.
            if name == "major-verdict" {
                majors
                    .entry(issued.generation)
                    .or_default()
                    .insert(issued.verdict.clone());
            } else {
                judges.entry(issued.generation).or_default().insert(
                    issued
                        .basis(version)
                        .is_ok()
                        .then(|| serde_json::to_string(issued).expect("issued JSON")),
                );
            }
        }
    }
    result.judgment_generation = judges.last_key_value().map(|(g, _)| *g);
    result.major_generation = majors.last_key_value().map(|(g, _)| *g);
    result.major = latest(majors).and_then(|v| match v.as_str() {
        "ok" => Some(Major::Cleared),
        "held" => Some(Major::Held),
        _ => None,
    });
    result.judgment = latest(judges)
        .flatten()
        .and_then(|v| serde_json::from_str::<Issued>(&v).ok())
        .and_then(|issued| {
            if issued.provenance.as_ref().is_some_and(|p| !p.valid()) {
                return None;
            }
            // Only a v3 issuance carries why, and every v3 one does.
            let version = if issued.why.is_some() {
                Version::V3
            } else {
                Version::V2
            };
            let basis = issued.basis(version).ok()?;
            let verdict =
                legacy_word(&issued.verdict).or_else(|| Judgment::from_word(&issued.verdict))?;
            Some(Judged {
                revision: current,
                verdict,
                provenance: issued.provenance,
                basis,
            })
        });
    result
}

/// The words at the latest generation. Duplicates are idempotent;
/// conflicting authenticated words at one generation fail closed, rather
/// than allowing comment order to choose.
fn latest<T: Ord>(
    map: std::collections::BTreeMap<u64, std::collections::BTreeSet<T>>,
) -> Option<T> {
    map.into_iter().next_back().and_then(|(_, words)| {
        (words.len() == 1).then(|| words.into_iter().next().expect("one verdict"))
    })
}

/// Markers the steward wrote before the contract's words are read once.
/// Delete with the steward (Conary#256 slice 5).
fn legacy_word(word: &str) -> Option<Judgment> {
    match word {
        "land" => Some(Judgment::Pass),
        "failed" => Some(Judgment::Fail),
        _ => None,
    }
}

fn verified(expected: String, tag: &str) -> bool {
    bool::from(expected.as_bytes().ct_eq(tag.as_bytes()))
}
