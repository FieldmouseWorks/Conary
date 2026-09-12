---
last_updated: 2026-09-11
revision: 1
summary: Closeout for the 2026-09-06 line-cap exception audit, mapping findings to issues and PRs and recording deferred enforcement
---

# Line-cap exception audit closeout

Closes the audit opened 2026-09-06 against `scripts/line-cap-allowlist.txt` and
the `conary-xtask` line-cap gate. Findings, PRs, and proof are mapped here;
issue bodies and PR descriptions remain the owners of detail.

## Findings and disposition

| Finding | Issue | Disposition |
| --- | --- | --- |
| Two exceptions cited CLOSED #814 | #852 | Fixed: entries retired; #852 reassigned as owner |
| Gate exempted test files by path name alone | #997 | Addressed by report-only classification (#1012) |
| Ordinary modules reported as test "reduction" | #998 | Fixed: `attributed_test_lines`, `reduction` removed (#1013) |
| `third_party/` unscanned; roots unstated | #999 | Fixed: typed root policy and vendor exclusion (merged) |
| Allowlist citations never checked against issue state | #1000 | Fixed: content-bound snapshot (merged) |

## Audit-found defects outside the original scope

| Defect | Issue | State |
| --- | --- | --- |
| Catalog RSS test wrote scratch into the source tree | #1001 | Open |
| Sandbox tests do not skip when mount namespaces are unavailable | #1003 | Open |
| Catalog peak-RSS bound unstable under concurrent load | #1004 | Open |
| `check-line-cap.sh` substitutes the repository snapshot for caller arguments | #1011 | Open, specified |

## What is deliberately not done

**Exemption enforcement is deferred.** The classifier reports; it decides no
cap outcome. Enforcing on a classifier that is merely content-aware would
produce roughly 200 false failures in this repository, and one that ignores
compilation contexts would misclassify production-imported files. The
acceptance criteria for enforcement stay open in #997.

A checked-in snapshot cannot detect an issue closed on GitHub while local files
are unchanged. That is a limit of a hermetic gate, not a defect, and needs an
issue-event, scheduled, or explicit networked check if the stronger property is
required.

## Proof retained

- `native_transaction.rs` reports `siblings=2 attributed_test_lines=1884` — its
  two test-gated children only. Its 2,695 lines of production children are
  excluded; the previous implementation printed 4,579.
- `cargo_test_target_with_a_production_import_is_not_test_only` asserts
  `Ungated` for a `#[path]`-imported test-target file reached from a production
  `lib.rs`.
- `EXEMPT SUMMARY: test-gated=425 ungated=0 unknown=0`. Zero `unknown` is a
  measurement, not a target: a non-zero count is information, not a regression.
- The split preserved the combined implementation:
  `git diff --exit-code --no-textconv 4124ca2b feat/1009-sibling-attribution`
  exits 0, against an expected tree built independently from the combined
  checkpoint plus the accepted documentation patch.

## Superseded PRs

#1007, #1008, #1009, #1010 were closed rather than merged: their heads predated
the #1002/#1005 rebase. Their replacements are #1012 and #1013 in the order
`#1006 → #1012 → #1013`.
