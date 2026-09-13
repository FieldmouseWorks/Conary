# Third-Party Divergence Inventory

This file is the single inventory for Cargo dependencies whose source differs
from their published upstream. The TOML block is both the human audit record
and the input to `scripts/check-third-party-divergence.py`; do not maintain a
second list.

The structural check runs in documentation truth plus every pull-request and
post-merge dependency-consistency job. The upstream exit check runs in the
scheduled security audit every six hours. When an upstream exit condition is
met, that audit fails and the owning dependency must be tested and either
returned to upstream authority or deliberately re-pinned with this record
updated.

<!-- conary-third-party-divergence:start -->
```toml
schema = 1
cadence = "Every six hours in the scheduled-ops audit job"
owner = "Cargo.toml dependency reviewers and the scheduled-ops audit job"

[[dependency]]
id = "aws-creds-quick-xml"
cargo_name = "aws-creds"
kind = "crates-io-patch"
declaration = "Cargo.toml:[patch.crates-io].aws-creds"
path = "third_party/aws-creds-0.39.1-patched"
baseline = "0.39.1"
upstream = "https://crates.io/crates/aws-creds"
upstream_index = "aw/s-/aws-creds"
divergence = "Raises the normal quick-xml dependency from 0.38 to 0.41; the remaining crate source is the crates.io 0.39.1 release."
reason = "Avoid shipping quick-xml releases covered by RUSTSEC-2026-0194 and RUSTSEC-2026-0195."
exit_type = "crates-io-dependency-floor"
exit_dependency = "quick-xml"
exit_requirement = ">=0.41"
exit_condition = "Drop the patch when a newer non-yanked aws-creds release requires quick-xml >=0.41 and the workspace resolves and passes its normal gates without the override."
exit_test = "cargo test --workspace --exclude conary-test"

[[dependency]]
id = "rust-s3-quick-xml"
cargo_name = "rust-s3"
kind = "crates-io-patch"
declaration = "Cargo.toml:[patch.crates-io].rust-s3"
path = "third_party/rust-s3-0.37.2-patched"
baseline = "0.37.2"
upstream = "https://crates.io/crates/rust-s3"
upstream_index = "ru/st/rust-s3"
divergence = "Raises the normal quick-xml dependency from 0.38 to 0.41; the remaining crate source is the crates.io 0.37.2 release."
reason = "Avoid shipping quick-xml releases covered by RUSTSEC-2026-0194 and RUSTSEC-2026-0195."
exit_type = "crates-io-dependency-floor"
exit_dependency = "quick-xml"
exit_requirement = ">=0.41"
exit_condition = "Drop the patch when a newer non-yanked rust-s3 release requires quick-xml >=0.41 and the workspace resolves and passes its normal gates without the override."
exit_test = "cargo test --workspace --exclude conary-test"

[[dependency]]
id = "resolvo-conflict-graph"
cargo_name = "resolvo"
kind = "crates-io-patch"
declaration = "Cargo.toml:[patch.crates-io].resolvo"
path = "third_party/resolvo-0.12.1-patched"
baseline = "0.12.1"
upstream = "https://crates.io/crates/resolvo"
upstream_index = "re/so/resolvo"
divergence = "Filters root-unreachable conflict evidence and backports upstream PR #293 (dfb403fce20c989a150ef5bbf89c2d2c1e2b3199) to group mixed-name virtual providers by concrete name."
reason = "Upstream 0.12.1 still panics on unrelated conflict evidence and incorrectly groups virtual capability providers in at-most-one clauses."
exit_type = "crates-io-newer-release"
exit_condition = "Evaluate every newer non-yanked resolvo release; drop the patch only when the conflict-graph and mixed-name-provider regressions passes against that release."
exit_test = "cargo test --manifest-path third_party/resolvo-0.12.1-patched/Cargo.toml --lib conary_tests"

[[dependency]]
id = "tantivy-lru"
cargo_name = "tantivy"
kind = "crates-io-patch"
declaration = "Cargo.toml:[patch.crates-io].tantivy"
path = "third_party/tantivy-0.26.2-patched"
baseline = "0.26.2"
upstream = "https://crates.io/crates/tantivy"
upstream_index = "ta/nt/tantivy"
divergence = "Raises the normal lru dependency from 0.16.3 to 0.18.2 in the normalized Cargo.toml, matching upstream PR #3034 (5ca39332002c2c87fb5d2abc707cf527b3319d42), and updates the standalone test lockfile to lru 0.18.4; all other files are the crates.io 0.26.2 release (archive SHA-256 861facfabd71044968f364837f9a083b56464ba5a59079f88706ee5c451ca069)."
reason = "Avoid shipping lru releases covered by RUSTSEC-2026-0253 through Remi search; tracked in #1019."
exit_type = "crates-io-dependency-floor"
exit_dependency = "lru"
exit_requirement = ">=0.18.2"
exit_condition = "Drop the patch when a newer non-yanked Tantivy release requires lru >=0.18.2 and Remi passes its package tests without the override."
exit_test = "cargo test -p remi"

```
<!-- conary-third-party-divergence:end -->

The vendored crates retain their upstream license metadata and notices. The
resolvo patch has additional implementation context in
[`resolvo-0.12.1-patched/CONARY_PATCH.md`](resolvo-0.12.1-patched/CONARY_PATCH.md).
