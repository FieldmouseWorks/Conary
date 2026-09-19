// apps/conary-test/src/explorer/checker.rs

use super::contract::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Expected bytes come from reviewed fixture contracts, never the product DB.
#[derive(Default)]
pub struct Oracle(pub BTreeMap<Package, Fixture>);

impl Oracle {
    pub fn accept(&mut self, receipt: &Receipt) {
        if receipt.exit_code != 0 {
            return;
        }
        match receipt.action {
            Action::Install(fixture) => {
                self.0.insert(fixture.package(), fixture);
            }
            Action::Remove(package) => {
                self.0.remove(&package);
            }
            _ => {}
        }
    }
    pub fn evaluate(&self, facts: &Facts, negative: bool) -> Vec<Evaluation> {
        let mut results = Vec::new();
        for package in [Package::App, Package::Companion] {
            let expected = self.0.get(&package);
            let version = expected.map(|f| f.version().to_owned());
            let owner = expected.map(|_| package.name().to_owned());
            let mut hash = expected.map(|f| hex::encode(Sha256::digest(f.payload().as_bytes())));
            // Deliberately wrong expectation, explicitly a checker control.
            // No product bytes or production guards are weakened.
            if negative && package == Package::App && expected.is_some() {
                hash = Some("0".repeat(64));
            }
            for (kind, passed) in [
                ("version", facts.packages.get(&package) == version.as_ref()),
                ("owner", facts.owners.get(&package) == owner.as_ref()),
                ("payload", facts.payloads.get(&package) == hash.as_ref()),
            ] {
                let control = negative && package == Package::App && kind == "payload";
                results.push(Evaluation {
                    criterion: format!("{}.{kind}", package.name()),
                    checker: CHECKER.into(),
                    classification: if !facts.complete {
                        Classification::Inconclusive
                    } else if control && !passed {
                        Classification::NegativeControl
                    } else if passed {
                        Classification::Pass
                    } else {
                        Classification::ProductFailure
                    },
                    passed: facts.complete.then_some(passed),
                    detail: if control {
                        "labelled negative checker control; not #917 reproduction"
                    } else {
                        "fixture contract versus independent DB/file observation"
                    }
                    .into(),
                });
            }
        }
        results
    }
}
