---
last_updated: 2026-09-10
revision: 40
summary: Record immutable v0.17.2 security publication and independent artifact proof while preserving blocked production and tester gates
---

# Release Artifact Matrix

This matrix is the limited-preview artifact contract. It distinguishes Cargo
package ownership, artifact construction, a prepared release target, immutable
publication, deployment, and independent runtime proof. Artifact URLs,
checksums, signatures, deployments, and behavior become authority only after
their exact evidence is recorded here.

Remote Forge validation and conary-test deployment are decommissioned. Local
QEMU/KVM evidence may support a preview row only when it names the absolute run
date, distro, suite, and pass counts.

## Current Release Suite

Nightly selection first discovers any valid typed nightly tag for today's UTC
date, regardless of its stable base. That existing annotated tag selects its
peeled commit before any green-run API lookup; the summary records
`selected_by_existing_date_tag`, the tag, date, and commit. It resumes through
`tag_without_release`, `draft_release`, `published_without_proof`, or `proved`
even when a newer commit has turned green. Malformed lookalikes remain ignored
non-authority. Multiple valid tags for the date fail as `ambiguous_nightly_date`.
Only an absent date tag permits green-commit selection and the new-tag preflight
below. Existing-tag recovery does not require an older commit to contain a newer
running workflow revision. Selection pins the UTC date for the whole run, so a
midnight rollover during preflight cannot change its intended tag date.

Before creating a nightly tag, `nightly-release.py preflight` verifies the
selected checkout matches the green commit and contains the running workflow's
`github.sha` as an ancestor. The selected tree's own `release.sh suite --dry-run`
must produce the stable base; its matrix must validate the full nightly version
and render every target, and its `release.sh suite --dry-run --target <nightly>`
must accept that target. These checks do not prepare or rewrite the tree.
A commit older than the running workflow or lacking these capabilities reports
`state: unsupported_commit`, `outcome: skipped`, the selected commit, and the
failed capability in the step summary. The selection step exits successfully
before recovery, build, or any tag-creation API call. Git operational failures
and a mismatched checkout remain typed failures rather than unsupported content.

The signed bootstrap installer reads Debian versions from the verified local
package and the installed dpkg database. Only when `dpkg --compare-versions`
reports the requested version as older does the exact local `apt-get install -y`
transaction add `--allow-downgrades`; absent, equal, and newer requests do not.
Fedora keeps `dnf install -y <exact-local-rpm>`: DNF5 already installs the exact
requested version regardless of the installed version, including downgrades.
Neither `dnf downgrade` nor `--allowerasing` is necessary; the latter permits
dependency removals, and `--allow-downgrade` controls dependencies, not the exact
target. This follows the pinned [DNF5 5.2.17.0 install documentation](https://github.com/rpm-software-management/dnf5/blob/043c5d1152a5adb2eaf3031620e49a659a0040ee/doc/commands/install.8.rst).
Arch retains `pacman -U --noconfirm -- <exact-local-package>`, which already
permits native downgrades. Full signed suite identity remains mandatory for
both the installed-version short-circuit and post-install health check.

During historical nightly tag discovery, tags rejected by the matrix grammar
are non-authority: `ignored_malformed_tag` and the tag name are recorded in the
step summary and on stderr. They cannot be selected, deleted by retention, or
block valid nightly creation/recovery. Explicit requested tags remain strictly
validated. Resolution stdout remains one typed JSON result for workflow callers.
Release notes use the same validation through `nightly-release.py notes-boundary`.
Excluding the current tag, the newest valid nightly by creator timestamp wins;
ties use the numeric stable version and nightly date, independent of listing
order. If none is valid, the latest validated stable tag by the same ordering
is the boundary. The step summary records `previous_tag_name`, `boundary_channel`,
and `fallback_to_stable`; no valid boundary fails as `notes_boundary_missing`
instead of delegating selection to GitHub's implicit boundary heuristics.

Issue [#428](https://github.com/FieldmouseWorks/Conary/issues/428) established the
current hard-cut topology: all eight Cargo packages inherit one root workspace
version; four artifact products are built from one reviewed suite commit; and
one annotated `vMAJOR.MINOR.PATCH` tag publishes one GitHub release. Conary and
Remi retain protected deployment lanes. conaryd and conary-test remain
build-only artifacts with `deploy_mode=none`. Suite metadata is a schema-v1
JSON resource; `dry_run` is a boolean rather than release authority encoded as
free-form text. Every member also inherits `publish = false`; the workspace has
no independent Cargo-registry publication track.
GitHub release immutability is enabled: publishing the fully populated draft
locks its tag and assets and creates the release attestation required by
independent closeout proof. The `Protect suite tags` ruleset separately permits
new `v*` tags while rejecting updates and deletions from the moment each tag is
created.

Version `0.17.2` is the current immutable release authority. Annotated tag
object `1e4045b57b43d1776fa06e30cc5e5f2a138fca87` peels to reviewed merge commit
`1a838a4af3c756f40c86fd459b7f68cb27aece72`; all four products were published
together and independently verified within the artifact-proof scope recorded
below. Deployment routing passed; terminal live deployment proof remains
pending behind the no-active-universe completion defect
[#927](https://github.com/FieldmouseWorks/Conary/issues/927) and public promotion
[#598](https://github.com/FieldmouseWorks/Conary/issues/598). Historical releases
remain immutable evidence for their own trees, but are not current release inputs.

The suite includes the privileged build-sandbox repair from
[PR #989](https://github.com/FieldmouseWorks/Conary/pull/989).
[GHSA-6qhh-qcc5-fxxg](https://github.com/FieldmouseWorks/Conary/security/advisories/GHSA-6qhh-qcc5-fxxg)
is public and identifies `0.17.1` as affected and `0.17.2` as patched.
Publication does not activate a public universe or assign tester authority.

The v0.16.1 release-era deployment ran the exact tagged `remi 0.16.1` binary
whose release asset has SHA-256
`64452867a6b3dab69df6ffd6b2610379321247de3abb2be07a62b4089eb9959d`.
That historical deployment proof recorded schema revision 40, 6/6 populated
sources, 110,182 repository packages, 3,855 conversions, and all four signing
profiles. It is not a claim about the current production schema or active
public universe. Broad external outreach remains separately postponed at 0/10
qualifying completions behind the current gates in
`docs/roadmaps/launch-status.json`; release proof is not tester authority.

The suite retains two Conary product assets:
`conary-bootstrap-v1.manifest` and its detached `.sig`. The release workflow
constructs the manifest only after the exact RPM, DEB, and Arch packages exist;
it binds the suite tag/version and each supported host to one exact basename,
size, and SHA-256, then signs the manifest with the same Ed25519 release key
embedded for self-update authority. `site/static/install-conary-preview.sh`
verifies that signature before parsing any selection field and verifies the
selected artifact before a native package transaction. Exact-tag release-build
and released-artifact proof both completed the clean three-host bootstrap path.

The historical `v0.17.1` suite was the first release under the license split from
[#905](https://github.com/FieldmouseWorks/Conary/pull/905): the Conary client
and libraries are `MIT OR Apache-2.0`, and Remi is `AGPL-3.0-or-later`. The
release includes `LICENSE-MIT`, `LICENSE-APACHE`, and `LICENSE-AGPL-3.0-remi`.
Tester authority remains unassigned in `docs/roadmaps/launch-status.json`.
Protected tag `v0.16.0` remains reserved evidence for a failed
version-validation run and has no GitHub release; it was not moved or reused.
Protected tag `v0.17.0` remains reserved evidence for a failed
packaged-license proof run (release-build 33989192427 stopped at `build-ccs`
because the proof passed `--policy` to `conary ccs inspect`) and has no GitHub
release; it was not moved or reused, and v0.17.1 carries the same content plus
the proof fix.

## Nightly Pre-Release Channel

The typed `stable` channel uses `MAJOR.MINOR.PATCH`. The typed `nightly`
channel uses `MAJOR.MINOR.PATCH-nightly.YYYYMMDD`, and the suffix must name a
real UTC calendar date. At `30 6 * * *`, or on manual dispatch,
`.github/workflows/nightly-release.yml` selects the newest `main` commit whose
`merge-validation` run concluded successfully. It computes the next stable
base from `scripts/release.sh suite --dry-run`, creates one annotated
`v<version>` tag through the GitHub REST API, and calls the shared release
build with `channel: nightly`.

Nightly package construction first runs `scripts/release.sh suite
--prepare-only --target <full-nightly-version>` in the runner checkout. The five
authority files receive the ecosystem renderings of that full version; a nightly
never rewrites or commits them on `main`. `assert-owned-version` checks each
file against its rendered value and is read-only. Release metadata records both
the full nightly version and its stable base; the base is informational, never
package or binary identity. The published GitHub release is
marked as a prerelease, its notes list merged pull-request titles since the
previous nightly tag, and `release-artifact-proof` runs against the published
tag. Tags created with `GITHUB_TOKEN` intentionally do not trigger the
tag-push release workflow a second time.

`scripts/release-matrix.sh render-version <version> <target>` is the single
typed rendering owner. It returns a raw version value, without field labels or
the tag's `v` prefix. Stable `0.17.0` renders as `0.17.0` on every target.
The pinned upstream rules and expected ordering for this nightly are:

| Target | Rendering of `0.17.0-nightly.20260905` | Ordering against `0.17.0` and pinned authority |
| --- | --- | --- |
| `cargo` | `0.17.0-nightly.20260905` | Lower: [SemVer 2.0.0 sections 9 and 11](https://semver.org/spec/v2.0.0.html#spec-item-9) |
| `rpm` | `0.17.0~nightly.20260905` | Lower: tilde prerelease operator, [RPM 6.0.2 rpm-version(7), 2026-08-20](https://rpm.org/docs/6.0.x/man/rpm-version.7) |
| `deb` | `0.17.0~nightly.20260905` | Lower: tilde precedes even an empty part, [Debian Policy 4.7.4.1 section 5.6.12](https://www.debian.org/doc/debian-policy/ch-controlfields.html#version) |
| `arch` | `0.17.0nightly20260905` | Lower: alphabetic suffix directly after the numeric segment, [pacman 7.1.0 vercmp(8), 2026-05-06](https://man.archlinux.org/man/core/pacman/vercmp.8.en) |
| `ccs` | `0.17.0-nightly.20260905` | Lower: Conary version scheme uses SemVer 2.0.0 |
| `tag` | `0.17.0-nightly.20260905` | Canonical tag is `v0.17.0-nightly.20260905`; no package ordering |

RPM writes `Version: 0.17.0~nightly.20260905`; Arch writes
`pkgver=0.17.0nightly20260905`. The Arch rendering uses neither a hyphen nor
a tilde; see [pacman 7.1.0 PKGBUILD(5), pkgver](https://man.archlinux.org/man/core/pacman/PKGBUILD.5.en).
The ordering table pins the expected RPM/pacman behavior when their comparison
tools are absent from the runner. Tests assert every rendering exactly and run
`dpkg --compare-versions '0.17.0~nightly.20260905' lt '0.17.0'` with the real
Debian comparator; they also use `vercmp` when installed. No local comparator
substitutes for an ecosystem's ordering implementation.

Immutable source pins for those native manuals are
[RPM 6.0.2, `a7f89afb`](https://github.com/rpm-software-management/rpm/blob/a7f89afb57a98f6419d0eff35ca792198293b682/docs/man/rpm-version.7.scd),
[Debian Policy 4.7.4.1, `f2b46743`](https://salsa.debian.org/dbnpolicy/policy/-/blob/f2b46743e3fb22fb3f461c28ffff4c2788a75bed/policy/ch-controlfields.rst),
and pacman 7.1.0, `5683f847`:
[vercmp(8)](https://gitlab.archlinux.org/pacman/pacman/-/blob/5683f8477a0afcc6b331766175a83445b2dcfe89/doc/vercmp.8.asciidoc)
and [PKGBUILD(5)](https://gitlab.archlinux.org/pacman/pacman/-/blob/5683f8477a0afcc6b331766175a83445b2dcfe89/doc/PKGBUILD.5.asciidoc).

CCS manifest validation delegates to `repository::versioning::validate_repo_version`
with `VersionScheme::Conary`, which parses `semver::Version`.
`self_update::versioning` likewise parses and compares full SemVer values.
Both already accept prereleases and order them below the matching stable
release. No CCS grammar, persisted schema, compatibility adapter, or rebuild
requirement changes in this correction.

Native source archives and build outputs use their rendered package version.
Release-page basenames use the full suite version (`conary-0.17.0-nightly.20260905...`),
so bundling normalizes the three native filenames without changing package
metadata. Cargo and CCS retain the full SemVer version throughout. All four
binaries report the full suite version; the preview installer's health check
and `release-artifact-proof` require that exact identity, including the date.

The nightly workflow uses `scripts/nightly-release.py` to resolve the selected
commit's state. Tag existence alone is never completion. The step summary
records schema-versioned JSON with `state`, `outcome`, tag, commit, and release ID.

| State | Outcome | Action |
| --- | --- | --- |
| `no_tag` | `build` | Create the annotated tag, build, publish, then prove |
| `tag_without_release` | `build` | Reuse the existing tag, build, publish, then prove |
| `draft_release` | `build` | Resume the draft publication and run proof |
| `published_without_proof` | `proof` | Run `release-artifact-proof` only; never replace published assets |
| `proved` | `skipped` | No build or proof work needed |

The aggregate `release-artifact-proof` job emits a versioned Actions receipt
only after all native lifecycle jobs succeed. Its artifact name binds the
immutable release ID, peeled commit, and run attempt. Recovery accepts it only
from the nightly or artifact-proof workflow on reviewed `main`, with a successful
terminal proof job in that exact attempt and receipt timestamps within that job.
An absent, expired, failed, stale, or differently bound receipt causes proof to
run again. This receipt represents the existing native artifact-proof scope,
not additional real-mount or production proof. Reusable release invocations
are detected from typed nightly channel/tag inputs, never the caller event name.

All releases, including nightly prereleases, remain immutable. Retention deletes
whole published nightly prerelease records older than 14 days, including their
assets; it never deletes individual assets or protected tags. GitHub explicitly
permits [deleting a whole immutable release](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases#what-immutable-releases-protect).
Immutability is repository-wide and has no per-release opt-out. A rejected
DELETE fails with typed `retention_delete_failed`, `release_id`, and `api_status`
(the HTTP status, or `transport_error` when no HTTP response was received).
Successful deletion records `release_deleted` and status 204. Drafts, stable
releases, and releases exactly at the 14-day boundary are retained.

A nightly is automated validation
evidence and is not a production candidate: it does not deploy Conary, Remi,
conaryd, or conary-test, and it does not become external-tester authority.

Between suite releases, `.github/workflows/build-remi-candidate.yml` constructs
one exact release-profile Remi artifact for every protected `main` commit. Its
schema-v2 manifest binds the source tree, lockfile, toolchain, build command,
flags, runner provenance, binary and deterministic bundle digests, bounded
local-bulk compiler-cache policy and statistics, and attributable compiler,
phase, and link timings. One compatible prior snapshot is restored in bulk,
all compilation is local, and the completed exact-head snapshot is saved once;
the cache remains an optimization rather than artifact authority. The candidate deployment lane accepts
only a successful `push` artifact for the requested SHA on this repository's
`main`, reopens and verifies the bundle, and enforces a 60-second
locate/download/verify budget. It never compiles Remi itself. These artifacts
are deployment candidates, not tags, releases, or substitutes for the
synchronized suite authority below.

| Artifact product | Artifact classes | Current construction authority | Suite deploy mode | Current immutable authority | Local build |
| --- | --- | --- | --- | --- | --- |
| `conary` | binary, `.ccs`, `.rpm`, `.deb`, `.pkg.tar.zst`, signed bootstrap manifest | `.github/workflows/release-build.yml`, `scripts/release.sh suite`, `scripts/release-matrix.sh` | protected release assets, static sites, and released-package proof | synchronized suite `v0.17.2`; detached signatures for the CCS artifact and bootstrap manifest | `cargo build -p conary` |
| `remi` | binary and tarball | `.github/workflows/release-build.yml` for suites; `.github/workflows/build-remi-candidate.yml` for build-once exact-main candidates; `scripts/release.sh suite`, `scripts/release-matrix.sh` | protected Remi deployment and repopulation proof, serialized before Conary deployment | synchronized suite `v0.17.2`; released binary digest verified; live deployment proof pending | `cargo build -p remi` |
| `conaryd` | binary and tarball | `.github/workflows/release-build.yml`, `scripts/release.sh suite`, `scripts/release-matrix.sh` | `none` | synchronized suite `v0.17.2`; build-only route | `cargo build -p conaryd` |
| `conary-test` | binary and tarball | `.github/workflows/release-build.yml`, `scripts/release.sh suite`, `scripts/release-matrix.sh` | `none` | synchronized suite `v0.17.2`; build-only route | `cargo build -p conary-test` |

## Recorded Evidence

### Conary 0.17.2 security suite

- Security PR [#989](https://github.com/FieldmouseWorks/Conary/pull/989) passed
  all 41 hosted checks and merged as reviewed commit
  `1a838a4af3c756f40c86fd459b7f68cb27aece72`. Protected annotated tag `v0.17.2`
  has tag object `1e4045b57b43d1776fa06e30cc5e5f2a138fca87` and peels to that
  exact merge commit.
- Exact-tag release-build
  [34415794323](https://github.com/FieldmouseWorks/Conary/actions/runs/34415794323)
  passed all 14 applicable jobs (nightly-only proof skipped), publishing the
  immutable 18-asset [v0.17.2 release](https://github.com/FieldmouseWorks/Conary/releases/tag/v0.17.2)
  at `2026-09-09T23:58:48Z`. Independently downloaded SHA-256 digests:
  - `LICENSE-AGPL-3.0-remi`:
    `0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0`
  - `LICENSE-APACHE`:
    `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`
  - `LICENSE-MIT`:
    `3681cd22a07cf6ae3dbadde8a04fde421b28f30c9c378e8031874ab95025cd83`
  - `SHA256SUMS`:
    `22082cbd6c7c3dd35ff7f843ba77fb0591bcb7029323e48d5a8e55553f5e1ab4`
  - `conary-0.17.2-1-x86_64.pkg.tar.zst`:
    `1524939c713797ff1d0945c443d71e3f03b593fb2b697a8ef4bde6f9b19a1196`
  - `conary-0.17.2-1.fc44.x86_64.rpm`:
    `a42aacea65d81e2d0f27f18fb8af629f9162b8ed8a569d86b478dbcb29aa025d`
  - `conary-0.17.2.ccs`:
    `c8ff7fbd63d149477d349661e03c2e343bcdedfafbc60fc79b898c7ea1b5b7b0`
  - `conary-0.17.2.ccs.sig`:
    `710bede860c38522c761922ece6b54a954d6851cfb71c1d6c2034096a300e54d`
  - `conary-bootstrap-v1.manifest`:
    `7219f283e063da46b6501ea7410269c913edb506a26fe7d29cba633f4d10513f`
  - `conary-bootstrap-v1.manifest.sig`:
    `7b4466d27c7d5f91242ba7306257bd2ea383f08891d64651ae205c4e26204330`
  - `conary-test-0.17.2-linux-x64`:
    `88545a192c8b620cb88c56b7310540a237c9b3732304ec482f47be31a1833c2f`
  - `conary-test-0.17.2-linux-x64.tar.gz`:
    `21e097e91c089fcb49207ae4a1b60716331887ec0460d8b2d4d1ea7d8325260d`
  - `conary_0.17.2-1_amd64.deb`:
    `0f548d388ac64b4e9b397bd82869003a23ff3226b5cc4b54f28f22642bd6a613`
  - `conaryd-0.17.2-linux-x64`:
    `87201316760c25f30cc2aa00cd3349a410b00713182ff170ef9900b651bc8b3a`
  - `conaryd-0.17.2-linux-x64.tar.gz`:
    `2ba8bd0969041c9d8a943077c6d6cec374868a825cdde71bd260a2b4d8e17d2b`
  - `metadata.json`:
    `7213117225946271c6c82b467b233373645b46a7cca5cccf2045fcce267cbfd4`
  - `remi-0.17.2-linux-x64`:
    `55f73d9f23bb0b35346e42fa88149b96bec22ba4847b7ab907d28eb0ae927ca6`
  - `remi-0.17.2-linux-x64.tar.gz`:
    `ab6e472885227e531f5a5cbd5b9b93b29a953f6a0f328dbac7dadfc3f3c06be2`
- Independent downloads matched all 18 GitHub asset digests and all 17
  `SHA256SUMS` entries. `gh release verify v0.17.2` and `gh release verify-asset`
  passed for the release and all 18 assets. OpenSSL verified both detached
  Ed25519 signatures using only the public release key. The extracted Conary
  DEB binary and each downloaded Remi, conaryd, and conary-test binary reported
  exact version `0.17.2`.
- Schema-v1 metadata names release `suite`, tag `v0.17.2`, version `0.17.2`,
  bundle `suite-bundle`, typed `dry_run=false`, and exactly four product routes:
  Conary `release_bundle`, Remi `remote_bundle`, and `deploy_mode=none` for
  conaryd and conary-test.
- Public advisory
  [GHSA-6qhh-qcc5-fxxg](https://github.com/FieldmouseWorks/Conary/security/advisories/GHSA-6qhh-qcc5-fxxg)
  was published at `2026-09-10T00:38:46Z` after independent artifact verification;
  the release notes link to it.
- On 2026-09-10, the public HTTPS installer at
  [install-conary-preview.sh](https://conary.io/install-conary-preview.sh)
  matched the checked-in script exactly, SHA-256
  `1daa7263129a4ab1eaa11023b8018f95efc34dd81f3f8f95be0bb2458675f453`.
  Its default GitHub latest-release manifest matched the verified `v0.17.2`
  manifest above. This proves served bytes and release selection; the endpoint
  fetch itself is not a full newcomer journey.
- Published native artifact proof is being recorded from
  [34422047623](https://github.com/FieldmouseWorks/Conary/actions/runs/34422047623),
  using workflow authority `1a838a4af3c756f40c86fd459b7f68cb27aece72`.
- Deployment run
  [34419260966](https://github.com/FieldmouseWorks/Conary/actions/runs/34419260966)
  passed routing and build-only route checks. On 2026-09-10, `deploy-remi`
  remained in progress and public `/health/ready` returned HTTP 503 because
  Arch had no active immutable catalog revision. Release verification does not
  satisfy [#927](https://github.com/FieldmouseWorks/Conary/issues/927)'s deployment
  completion repair or [#598](https://github.com/FieldmouseWorks/Conary/issues/598)'s
  signed public-universe promotion. No terminal live deployment proof is claimed.
- `published_release.tester_authority` remains `false`; the tester pin remains
  unassigned. [#639](https://github.com/FieldmouseWorks/Conary/issues/639) retains
  the post-universe release and complete three-host public journey acceptance.

### Conary 0.17.1 synchronized suite

- Preparation PR [#924](https://github.com/FieldmouseWorks/Conary/pull/924)
  merged as reviewed commit
  `83376626f8f238e4763389ce2084d655fc4d6cf3`.
- Protected annotated tag `v0.17.1` has tag object
  `ff70eb6aefe18e8554c94ced4592b4727e99feac` and peels to that merge commit.
  The protected failed `v0.17.0` and `v0.16.0` tags remain unchanged and have
  no releases.
- Exact-tag release-build run
  [33994856405](https://github.com/FieldmouseWorks/Conary/actions/runs/33994856405)
  passed all 14 applicable jobs (the nightly-only proof job was skipped) and
  published the immutable 18-asset release
  [v0.17.1](https://github.com/FieldmouseWorks/Conary/releases/tag/v0.17.1) at
  `2026-09-05T22:52:44Z`:
  - `SHA256SUMS`:
    `d4fcbbc52ed95893629275484681886cfaebe86ce73318504073912673b8b86a`
  - `LICENSE-AGPL-3.0-remi`:
    `0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0`
  - `LICENSE-APACHE`:
    `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`
  - `LICENSE-MIT`:
    `3681cd22a07cf6ae3dbadde8a04fde421b28f30c9c378e8031874ab95025cd83`
  - `conary-0.17.1-1-x86_64.pkg.tar.zst`:
    `0e25b09ef476a7f25a8b1a74d83084278aaffc0d413bee465b67025027e9eac9`
  - `conary-0.17.1-1.fc44.x86_64.rpm`:
    `b6c1b7f8ef44ce83eb84af8854b49aaa834f4c34fe43eeb4494c781332fd67fd`
  - `conary-0.17.1.ccs`:
    `855b12c1475ecbb327c92ca1e6ef7029de2bce73e4bdc56a99e63931483818dc`
  - `conary-0.17.1.ccs.sig`:
    `497c1cacb784d3a9856f97e6d46a07099ebba846c1832492d619344f8b35ebc7`
  - `conary-bootstrap-v1.manifest`:
    `aab6d0066438b236b32376199a3b7e7d43f289d181e8e62516abd8f37d32bb5f`
  - `conary-bootstrap-v1.manifest.sig`:
    `e0e25596c918d28df44c7518318da593dc7c1f6a82457b4991b43e5bc285ba57`
  - `conary-test-0.17.1-linux-x64`:
    `2a6fb893c5d5e2981b56eb76905c32c361b5c9e355549dfce1db4832cbb11200`
  - `conary-test-0.17.1-linux-x64.tar.gz`:
    `a49f13a46d5679d06e168560a4fbf887f30b1f771f0980510fd3fa1555866776`
  - `conary_0.17.1-1_amd64.deb`:
    `037293b6b8d95c434f5e52ff03b3388db145e9cc5c522cca6355a88076e34870`
  - `conaryd-0.17.1-linux-x64`:
    `c1c13df6f276b7ccc575b681fac28344e8a83981d5be29dcced5ee4c7f6421d9`
  - `conaryd-0.17.1-linux-x64.tar.gz`:
    `566e1eadd75b5c16e06ffd1e57705f992f409f8d8b50bb29cd773520824870c8`
  - `metadata.json`:
    `7535cb371875a04828f6e1c8d2d2df609030b5a55a11f12109de2e66dcfe169a`
  - `remi-0.17.1-linux-x64`:
    `1898160486911f11b6a6929b6083ff59227edf69f1e72fe24e63cd07544c7c29`
  - `remi-0.17.1-linux-x64.tar.gz`:
    `e437da923f73c2eacf02d4e34edd92e373ea0f57e081279f70557bc446e46cb2`
- `gh release download v0.17.1 -p SHA256SUMS` supplied all 17 non-checksum
  asset digests. Every entry, plus the downloaded `SHA256SUMS` file's own
  SHA-256, matched `gh release view v0.17.1 --json assets` (18/18).
  A fresh independent download passed `sha256sum -c SHA256SUMS` for all 17
  non-checksum assets. `gh release verify v0.17.1` and
  `gh release verify-asset` for all 18 downloaded assets passed against the
  immutable release attestation.
- Schema-v1 metadata names release `suite`, tag `v0.17.1`, version `0.17.1`,
  bundle `suite-bundle`, typed `dry_run=false`, and exactly four product routes:
  Conary `release_bundle`, Remi `remote_bundle`, and `deploy_mode=none` for
  conaryd and conary-test. Protected deployment run
  [33997142651](https://github.com/FieldmouseWorks/Conary/actions/runs/33997142651)
  passed `validate-routing` and `verify-build-only-routes`, then failed on
  2026-09-06 because the deployment required repopulation with no active
  universe. Its claimed one-hour wait held the deployment group for over five
  hours. [#927](https://github.com/FieldmouseWorks/Conary/issues/927) owns the
  completion contract and deadline repair; this run proves routing only.
- Independent release-artifact-proof run
  [33998911117](https://github.com/FieldmouseWorks/Conary/actions/runs/33998911117)
  passed all three native-package lifecycle jobs and the aggregate
  `release-artifact-proof` job using workflow authority
  `3f5d0cd2720492a371920bca5dadf87300017094` against published tag `v0.17.1`:
  - `fedora44`: signed bootstrap and RPM installation passed; lifecycle 4/4,
    zero failures; published-binary corpus gate passed (3 cases).
  - `ubuntu-26.04`: signed bootstrap and DEB installation passed; lifecycle
    4/4, zero failures; published-binary corpus gate passed (3 cases).
  - `arch`: signed bootstrap and Arch package installation passed; lifecycle
    4/4, zero failures; published-binary corpus gate passed (3 cases).
  Each host proved the published binary rejects test hooks. Hook-free
  operations use that published binary; the four mutations use a separate
  integration binary with explicit test hooks. Released-byte mutation on real
  mounts remains the separate [#848](https://github.com/FieldmouseWorks/Conary/issues/848)
  proof boundary.
- The first release attempt, protected tag `v0.17.0`, failed in release-build
  [33989192427](https://github.com/FieldmouseWorks/Conary/actions/runs/33989192427)
  at `build-ccs`: the packaged-license proof passed `--policy` to
  `conary ccs inspect`. PR [#923](https://github.com/FieldmouseWorks/Conary/pull/923)
  corrected the proof; the failed tag was neither moved nor reused and has
  no GitHub release.
- The first `v0.17.1` artifact-proof attempt,
  [33997179128](https://github.com/FieldmouseWorks/Conary/actions/runs/33997179128),
  failed on all three hosts because the harness binary lacked the `test-hooks`
  feature and rejected `CONARY_TEST_COMMIT_TIMESTAMP` and
  `CONARY_TEST_GIT_COMMIT`. PR [#925](https://github.com/FieldmouseWorks/Conary/pull/925)
  enabled the feature for the separate harness; the successful proof above
  uses that reviewed fix without changing the immutable tag or release assets.
- This is the first published suite under the license split from
  [#905](https://github.com/FieldmouseWorks/Conary/pull/905): the Conary client
  and libraries are `MIT OR Apache-2.0`; Remi is `AGPL-3.0-or-later`.
  Release publication and artifact proof do not assign tester authority:
  `published_release.tester_authority` remains `false`, the tester pin remains
  unassigned, and the tester guide remains paused.

### Conary 0.16.1 synchronized suite

- Preparation PR [#489](https://github.com/ConaryLabs/Conary/pull/489) passed
  its exact-head checks and merged as reviewed commit
  `0fb961bacc6360107506371b16b7f0345ba6f927`.
- Protected annotated tag `v0.16.1` has tag object
  `0c90d578fd3dd7b58e0c9f8a04f80228e5f65396` and peels to that merge commit.
  The protected failed `v0.16.0` tag remains unchanged and has no release.
- Exact-tag release-build run
  [32199379608](https://github.com/ConaryLabs/Conary/actions/runs/32199379608)
  passed all 14 jobs and published the immutable 15-asset release
  [v0.16.1](https://github.com/ConaryLabs/Conary/releases/tag/v0.16.1) at
  `2026-08-19T00:38:11Z`:
  - `SHA256SUMS`:
    `c93933178e4f87bfa1f58c6d6a9aa11b9ef644d5ae8fa8ba128a0c4167532464`
  - `metadata.json`:
    `b7d7c12de7b8608ef1114b2e2d2bd287934917741a00bff54e60ee63e085d141`
  - `conary-0.16.1.ccs`:
    `4da19ee11885456b7068e08836b2a8f72ffedd622b363c3afaa60326e1a1922e`
  - `conary-0.16.1.ccs.sig`:
    `e1f6875e415b7dcb65f87fbcb5b7e31a63fbfbc35f62e954bcbdf7449b5ad110`
  - `conary-bootstrap-v1.manifest`:
    `c8ae10059020b4cef531d0f73d50b806361dc4c7bfabd3e99551599d109cc276`
  - `conary-bootstrap-v1.manifest.sig`:
    `51b8552be96e423fe4e14881bda3d0a3d189516b266a17b60ab7a2d9deffa3b2`
  - `conary-0.16.1-1.fc44.x86_64.rpm`:
    `63d30c0b188bb871431c0eeff51a5333a55f4ab418468ca59c96b0e64824d62e`
  - `conary_0.16.1-1_amd64.deb`:
    `4c5fd64a30b7b1ddeada014d45bbee3369bcaffd845064aa764119e5ff0deb7e`
  - `conary-0.16.1-1-x86_64.pkg.tar.zst`:
    `e0e2aa2c556ef0d4765b34483dc6011c6450f2c7000be0c28eb8411306d87a76`
  - `remi-0.16.1-linux-x64`:
    `64452867a6b3dab69df6ffd6b2610379321247de3abb2be07a62b4089eb9959d`
  - `remi-0.16.1-linux-x64.tar.gz`:
    `335a8566286759959481c19cd41c2b12942c72085d43784686ac72b94421357a`
  - `conaryd-0.16.1-linux-x64`:
    `d875b6c78a64738769112e0b7553b37ffbf1887390fc58f673699f4350e101d3`
  - `conaryd-0.16.1-linux-x64.tar.gz`:
    `7e2cf36f53fb552c2ee33f3cf0df6e64632702e83c54bc96ba7404a658d8580c`
  - `conary-test-0.16.1-linux-x64`:
    `e78324f40f3c9bdfba360e7c946414690f55d821aed83104e526d7f25abccf05`
  - `conary-test-0.16.1-linux-x64.tar.gz`:
    `4f6a29d62f52ca9bd985ab24bf1763143b844bfe9f72a095d7b8fda50c1b6a97`
- A fresh independent download passed `sha256sum -c SHA256SUMS` for all 14
  non-checksum assets. `gh release verify v0.16.1` verified the immutable
  release attestation and every asset digest.
- Schema-v1 metadata names release `suite`, tag `v0.16.1`, version `0.16.1`,
  bundle `suite-bundle`, typed `dry_run=false`, and exactly four product routes.
  The signed bootstrap manifest binds Fedora 44, Ubuntu 26.04 LTS, and Arch
  x86_64 to their exact native package basename, size, and SHA-256.
- Protected deployment and proof run
  [32201994359](https://github.com/ConaryLabs/Conary/actions/runs/32201994359)
  passed exact routing, both build-only routes, Remi and Conary deployment,
  three native-package lifecycle jobs, and their aggregate release-artifact
  proof. Each supported host installed Conary 0.16.1 through the signed
  bootstrap protocol and exposed package-owned repository state.
- The same deployment recorded Remi schema revision 40, 6/6 populated sources,
  four exact signing profiles, 110,182 repository packages, and 3,855
  conversions; the public readiness endpoint reported `ready=true`.
- Release and deployment proof remain distinct from tester authority. W7/#110
  later passed, but this historical suite predates the signed-universe client;
  the external tester pin remains unassigned behind the current launch-status
  gates.

### Conary 0.15.0 synchronized suite

- Preparation PR [#429](https://github.com/ConaryLabs/Conary/pull/429) passed
  exact-head gate `31762562819` and exact-head rehearsal `31763203456` at
  `9361d2af4bdcc83b190de2fc6bf95234d92ac86c`, then merged as reviewed commit
  `642750878d5a59a9aa27976347cafc6f9dd86cfd`. Post-merge validation
  `31766458093` passed at that exact commit.
- Protected annotated tag `v0.15.0` has tag object
  `83ef2d8a264cb49c5deb9e79e2a84a20e6883dab` and peels to that merge commit.
  The commit is reachable from `main`; active ruleset `Protect suite tags`
  (`20825313`) rejects updates and deletions of `v*` tags with no bypass.
- Exact-tag release-build run
  [31766900566](https://github.com/ConaryLabs/Conary/actions/runs/31766900566)
  passed all 11 jobs. It published the immutable 13-asset release
  [v0.15.0](https://github.com/ConaryLabs/Conary/releases/tag/v0.15.0) at
  `2026-08-14T04:23:49Z`:
  - `SHA256SUMS`:
    `f160f65291b7d4f8a8e8357f6bf7783526fe17885165a09419f6c8652bf4024d`
  - `metadata.json`:
    `6c4615e8e9faa101674d5f168261f7b47621dad8f8f6cd5a474917b743f59033`
  - `conary-0.15.0.ccs`:
    `8c5348be89d2c92b094498443d23782c88ff5d4deed888290939b4d73d39cc8f`
  - `conary-0.15.0.ccs.sig`:
    `1730816e0cf92f219692f80e0575a4ef13f536d1bbe200ef45103a789a421c86`
  - `conary-0.15.0-1.fc44.x86_64.rpm`:
    `3297e0e1e625a3d0eb51c68f6bbe715443f2a69fb27216e61ab21529c76fe060`
  - `conary_0.15.0-1_amd64.deb`:
    `61f6c6691c0997f42abb2d2c6b37ed1a699a5cedca6721729d36be43a165cf94`
  - `conary-0.15.0-1-x86_64.pkg.tar.zst`:
    `ae28c6562e82dbb17bbdddf783b916c3ac37a02d5720fbb6101629cfa5e5078d`
  - `remi-0.15.0-linux-x64`:
    `5638e4715a7d6f6b2aa75105b337b77b49953ab6b04e84cf809daaa439563cc4`
  - `remi-0.15.0-linux-x64.tar.gz`:
    `d9fc6efde106e2e4d4f253eeeca929fa144b94737dfe22da9779122b3e99f0d0`
  - `conaryd-0.15.0-linux-x64`:
    `1ab85520d0c870bcd6e7f5c5df687b3487db2d268ab13324ab15e422aa34c770`
  - `conaryd-0.15.0-linux-x64.tar.gz`:
    `be1d6a23e094d7ca14d1bc5faee1899cab6b8105c740114833936f9f2dbb62dd`
  - `conary-test-0.15.0-linux-x64`:
    `7a135284ddc16901317c2ea1c66566cb6857f7bd67800d44c39d3f63235421a7`
  - `conary-test-0.15.0-linux-x64.tar.gz`:
    `3bc8fa88d80df3cbf03fbbe125dbcf5f924add3a9412d399842ac232cfe25ce1`
- Independent download proof passed `sha256sum -c SHA256SUMS` for all 12
  non-checksum assets and matched every one of the 13 local SHA-256 values to
  the release API digest. `gh release verify v0.15.0` and
  `gh release verify-asset` for every asset passed against GitHub's immutable
  release attestation.
- Schema-v1 metadata names release `suite`, tag `v0.15.0`, version `0.15.0`,
  bundle `suite-bundle`, typed `dry_run=false`, and exactly four product routes:
  protected Conary and Remi deployment plus `deploy_mode=none` for conaryd and
  conary-test. All 11 artifact patterns were present.
- Downloaded Remi, conaryd, and conary-test binaries report `0.15.0`; each raw
  binary is byte-identical to the copy in its tarball. The Arch package reports
  `conary 0.15.0-1` for `x86_64`, and the released-package workflow confirmed
  the installed Conary version for all three native package formats.
- Protected deployment run
  [31769739765](https://github.com/ConaryLabs/Conary/actions/runs/31769739765)
  passed exact metadata routing, build-only-route proof, Remi deployment,
  Conary release/static-site deployment, and terminal native-package lifecycle
  proof on Fedora 44, Ubuntu 26.04 LTS, and Arch at the tagged commit.
- Independent Remi proof passed `scripts/remi-health.sh --full` at 10/10 and
  `inspect-remi --require-repopulated` at schema revision 37, 6/6 populated
  sources, four exact signing profiles, 110,220 repository packages, and 1,798
  conversions. The installed `remi 0.15.0` binary hash exactly matches the
  release asset. The public self-update endpoint serves version `0.15.0` and
  the exact released CCS hash.
- The detached `.ccs.sig` is the product-owned signature. Native packages,
  executable bundles, and metadata have GitHub's release attestation and
  checksum coverage but no separate detached signature, SBOM, or additional
  provenance sidecar.

### Superseded exact-main Remi candidate

- Before the synchronized release, protected candidate-deployment run
  `31751375620` completed successfully on
  2026-08-13 at exact merged commit
  `c5b13097ef8818ab2df050afdf93d8343994cca9`.
- Independent host proof found the active `/usr/local/bin/remi` reporting
  `remi 0.12.1` with SHA-256
  `e6c6b826b1df6c12e33391dcbf5abc88e2719ab2b63c46750188d40675a80ef3`.
- `conary-remi-deploy inspect-remi --require-repopulated` passed with schema
  revision 37, 6/6 populated sources, four exact signing profiles, 110,220
  repository packages, and 1,645 conversions. Solus contributes 11,907 eopkg
  packages and one validated conversion.
- `scripts/remi-health.sh --full` independently passed 10/10, and public Solus
  package conversion for `0ad` returned HTTP 200.
- This evidence proves that superseded deployment. It did not create a tag,
  GitHub release, or separate Remi version authority.

### Remi 0.12.1

- Historical lightweight tag `remi-v0.12.1` points directly to release commit
  `ad8537f93ed94da417ecb4b53dc12c978d985bf9`. It remains exact evidence but
  does not satisfy the synchronized suite's annotated-tag contract.
- Release-build run `31327202489` passed and published the immutable release on
  2026-08-09. Independently downloaded assets matched their GitHub digests:
  `metadata.json`
  `ee3b00e16e4fd6a95db6ebe69a891555cfff9797a81d7e8c6f62133eb37530a8`,
  raw binary
  `88e6b76c1310e64080cb297c50aff44219a19584ab69fa1c6c468ba35f357401`,
  and tarball
  `f701b063c5a39a6f61d3deb7117b90fc85ad6f7163c9a2e410ab778cd96efa9a`.
- The raw and tar-extracted binaries are byte-identical and report
  `remi 0.12.1`. The release has no detached signature, `SHA256SUMS`, SBOM, or
  provenance sidecar.
- Protected deploy-and-verify run `31328183745` passed. The later exact-main
  candidate deployment recorded above supersedes its production state without
  changing this release's immutable evidence.

### Conary 0.14.0

- Annotated tag object `c36c767c7169ff519a96dfdc7bedfa757211f334`
  peels to reviewed commit
  `fe23a604b64ea6f7cc87fce8298911e2245e027f`.
- Exact-tag release-build run `30409720307` passed and published the immutable
  release on 2026-07-29. Independent downloads matched `SHA256SUMS` and every
  GitHub digest:
  - `conary-0.14.0-1.fc44.x86_64.rpm`:
    `646d89bd8e6de86e8ec983d7cdba942ccfcd45cecc32a3e3efb69576a62dc09a`
  - `conary_0.14.0-1_amd64.deb`:
    `6cb55e510dfe2578af530ca89054458cc0b61e33870af09245d1f1e6069eae8d`
  - `conary-0.14.0-1-x86_64.pkg.tar.zst`:
    `bab913f539325122ba86fbcc8cdb62dc396657709c55fac78927ae4bbc9d88ec`
  - `conary-0.14.0.ccs`:
    `f38fa623880b383fd9d2b49a0b003d2885a826f78d51c874ef216363cc61b3fd`
  - `conary-0.14.0.ccs.sig`:
    `9b26ddc8496f23005f5c6c3906c956816d215a73d1d789d6e2062a0763d7975d`
  - `SHA256SUMS`:
    `5e7aef32d1dcc31fa5a3779b6cf283704cefa56c4e3e6ab5b842c233e0168c7a`
  - `metadata.json`:
    `dfd5e8bb1add27867ca8a631223ce4d7f5f5c649d6b7207373ddc0ddb665418c`
- Metadata identifies product `conary`, tag `v0.14.0`, version `0.14.0`, and
  `release_bundle` deployment. No SBOM or provenance sidecar is published.
- Protected deploy-and-verify run `30412130145` passed exact release-bundle
  deployment, self-update endpoint and static-site checks, published artifact
  proof, and installed-package lifecycle proof on Fedora 44, Ubuntu 26.04, and
  Arch.
- The released RPM reports `conary 0.14.0`. In a 2,048 MiB Fedora 44 KVM
  guest it synchronized 76,354 live Fedora packages in 194,288 ms, and the
  guest database contained exactly 76,354 repository-package rows.
- The supported-host generation fixes developed after this tag are not in
  `v0.14.0`. Their qcow2/ISO proof belongs to issue #137 and PR #151 and does
  not make this older artifact current tester authority.

### Remi 0.9.5

- Annotated tag object `f2bf17f0086a7f8ea4be3e032336551c4e6089c1`
  peels to reviewed commit
  `101dba655257f1ff3d1bee689d9c5ac8b2b68cbd`.
- Exact-tag release-build run `30583793501` passed and published the immutable
  release on 2026-07-30. Independent downloads matched every GitHub digest:
  `metadata.json`
  `4ee564321ab8826679e880f8a386e7da6bb94589cad35c6374e71a6538f3bd80`,
  raw binary
  `8333105420ca30de79a8f312758699a950e7e0f280e80cbebf20302a942cbe14`,
  and tarball
  `9144164b9e61e666cc3a801eb08c8ca2d02de2efa09902f3a9e98e2aacf5b40b`.
- The raw and tar-extracted binaries are byte-identical and report
  `remi 0.9.5`. This release publishes no detached signature, SBOM, or
  provenance sidecar.
- Protected deploy-and-verify run `30585462182` passed at the exact release
  commit. Independent public proof on 2026-07-31 passed full health 11/11;
  readiness reports `ready=true` at schema revision 23. Public stats reported
  83,885 packages, 3,494 downloads, three distributions, and 1,783 converted
  packages.

### conaryd 0.7.0 and conary-test 0.9.0

- Release-build runs `30225403876` and `30225404886` passed at their shared
  immutable commit; deploy-routing runs `30226301835` and `30226177794`
  selected `no-deploy-required`, as the matrix requires.
- Independent downloads matched GitHub checksums, raw and tarred binaries were
  identical, and the binaries reported their owned versions. conary-test
  exposed the native cross-source lifecycle and complete owned suite
  inventory.
- Neither build-only product publishes a detached signature, SBOM, or
  provenance sidecar.

Deploy-helper artifact publication uses CI-produced trust inputs as evidence:
`conary-remi-deploy deploy-conary` verifies staged `SHA256SUMS` before
installing release files, copies the verified checksum file into the installed
release directory, refuses symlinked trust inputs, and requires a sibling
`.ccs.sig` whenever a staged `.ccs` artifact is present. This does not make a
candidate release preview-supported without the recorded proof above.

## Evidence Command Block

Run these commands before publication and again where the proof depends on the
published tag:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p conary-test -- list
bash scripts/check-doc-truth.sh
bash scripts/check-release-matrix.sh
bash scripts/release-cargo-audit.sh
```

For each published suite release, also record:

- annotated tag object and peeled commit;
- publication time and complete asset names;
- workflow run IDs and terminal conclusions;
- independently downloaded asset SHA-256 values and matching GitHub digests;
- binary `--version` output from all four downloaded artifacts;
- signature, SBOM, and provenance status for each artifact product;
- signed bootstrap manifest inventory and clean Fedora, Ubuntu, and Arch
  installer proof when the release carries the bootstrap protocol;
- serialized build-only routing for conaryd and conary-test;
- Conary and Remi deployment and live-behavior proof required by their rows.

## Support Loop

First-wave tester instructions link the support-bundle command,
`.github/ISSUE_TEMPLATE/pre_alpha_feedback.md`, this matrix, and the evidence
command block.

The support bundle is local-only. On an installed host, run `sudo -v` first;
the script uses cached authorization only for allowlisted database-backed
diagnostics and stops before writing a bundle if authorization is unavailable.
Review the result before attaching it. Do not include `/etc/conary/trust`,
private keys, SSH keys, host-local credential files, raw logs, environment
dumps, or live `conary.db` files unless a maintainer explicitly asks for a
separately reviewed follow-up.
