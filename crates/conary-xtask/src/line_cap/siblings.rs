// crates/conary-xtask/src/line_cap/siblings.rs

//! Attribute extracted sibling test mass to its declaring parent.
//!
//! The cap policy requires a sibling extraction to reduce the parent "with that
//! reduction stated in the commit". That statement is unverifiable if the tool
//! cannot see the relation, so this module reports the measured size of the
//! child modules a parent declares on the parent's own `--report` row.
//!
//! Only a child whose test-only status the exemption gate actually establishes
//! contributes. An ordinary production child such as `mod implementation;` is
//! not extracted test mass, and summing every resolved child's length would
//! report it as though it were. `exemption` owns the module-declaration graph
//! and the gate resolution; this module consumes the resolved gate set, so the
//! file a parent attributes and the file the exemption classifier gates are
//! always the same file.
//!
//! The value reported is the current size of those child files, not a
//! git-verified before/after: a child's present length cannot establish how much
//! its parent shrank. It is therefore named `attributed_test_lines` and nothing
//! here claims a historical delta.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::exemption::{ExemptionGate, ModuleDeclaration, resolve_child_modules, resolved_gate};
use super::{FileMetrics, analyze_source};

/// The test mass a file's own row attributes to `mod name;` children that are
/// established as test-only.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub(crate) struct SiblingAttribution {
    /// Children that contributed to `attributed_test_lines`.
    pub(crate) siblings: usize,
    /// Total lines of those children, in their current state.
    pub(crate) attributed_test_lines: usize,
}

/// The parent's `--report` row: its own metrics, plus the sibling fields when at
/// least one child actually qualified.
pub(crate) fn report_row(
    relative: &str,
    metrics: FileMetrics,
    attribution: SiblingAttribution,
) -> String {
    let mut row = format!(
        "{relative}\ttotal={}\tproduction={}\tinline_test={}",
        metrics.total_lines, metrics.production_lines, metrics.inline_test_lines
    );
    if attribution.siblings > 0 {
        row.push_str(&format!(
            "\tsiblings={}\tattributed_test_lines={}",
            attribution.siblings, attribution.attributed_test_lines
        ));
    }
    row
}

/// Resolve a parent's top-level `mod name;` children to the files rustc would
/// load, keeping only scanned files.
pub(crate) fn child_modules(
    relative: &str,
    declarations: &BTreeMap<String, Vec<ModuleDeclaration>>,
    scanned: &BTreeSet<String>,
) -> Vec<String> {
    resolve_child_modules(relative, declarations, scanned)
}

/// Sum the measured mass of the children the exemption gate establishes as
/// test-only. A child that is production (`ungated`) or that the analysis
/// cannot place (`unknown`) is left out of both the count and the sum: only a
/// proven test-only child contributes to a proven test-only total. A child that
/// cannot be read or parsed is likewise left out; it is already reported as its
/// own scan error.
pub(crate) fn sibling_attribution(
    root: &Path,
    children: &[String],
    gates: &BTreeMap<String, ExemptionGate>,
    measured: &mut MeasuredFiles,
) -> SiblingAttribution {
    let mut attribution = SiblingAttribution::default();
    for child in children {
        if resolved_gate(gates, child) != ExemptionGate::TestGated {
            continue;
        }
        let Some(metrics) = measured.measure(&root.join(child)) else {
            continue;
        };
        attribution.siblings += 1;
        attribution.attributed_test_lines += metrics.total_lines;
    }
    attribution
}

/// Metrics for scanned files, filled on demand so a parent can measure a child
/// before the scan reaches it. `analyze_source` is a pure function of the file
/// bytes, so a child measured early and in its own turn always agree.
#[derive(Default)]
pub(crate) struct MeasuredFiles {
    metrics: BTreeMap<PathBuf, Option<FileMetrics>>,
}

impl MeasuredFiles {
    pub(crate) fn insert(&mut self, path: &Path, metrics: FileMetrics) {
        self.metrics.insert(path.to_path_buf(), Some(metrics));
    }

    pub(crate) fn measure(&mut self, path: &Path) -> Option<FileMetrics> {
        if let Some(metrics) = self.metrics.get(path) {
            return *metrics;
        }
        let metrics = fs::read_to_string(path)
            .ok()
            .and_then(|source| analyze_source(&source).ok());
        self.metrics.insert(path.to_path_buf(), metrics);
        metrics
    }
}
