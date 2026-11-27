# Standalone Linux installation and operator prerequisites

Conary runs on any standard Linux host without Nix or NixOS. It ships three
binaries (`cn`, `conaryd`, and `conary-mcp`), expects `git` on `PATH` for `cn`
and `conary-mcp` (`why` runs `git blame` in the caller's checkout; `conaryd`
runs no git itself), and reads all paths, tokens, and service settings from
flags, environment variables, and JSON configuration files.

## Installing

### Option 1: Release tarball

Tagged releases publish a Linux archive (`conary-<version>-<target>.tar.gz`)
and `SHA256SUMS` built by
[`.github/workflows/release.yml`](../.github/workflows/release.yml). Each
archive contains `bin/cn`, `bin/conaryd`, `bin/conary-mcp`, `LICENSE`, and
`systemd/conaryd.service` (test and example binaries such as
`conary-flow-test-plugin`, `conary-execution-test-plugin`, and
`conary-example-plugin` are excluded).

Download the archive and `SHA256SUMS` for a release tag, verify the checksum,
and install the binaries to `/usr/local/bin`:

```sh
VERSION=v0.1.0
TARGET=x86_64-unknown-linux-gnu
ARCHIVE="conary-${VERSION}-${TARGET}.tar.gz"

sha256sum -c SHA256SUMS
tar -xzf "$ARCHIVE"
sudo install -m 0755 "conary-${VERSION}-${TARGET}/bin/cn" /usr/local/bin/cn
sudo install -m 0755 "conary-${VERSION}-${TARGET}/bin/conaryd" /usr/local/bin/conaryd
sudo install -m 0755 "conary-${VERSION}-${TARGET}/bin/conary-mcp" /usr/local/bin/conary-mcp
```

Verify the installed binaries:

```sh
cn --help
conaryd --help
conary-mcp --help
```

### Option 2: Build from source with plain Cargo

Plain Cargo requires only `rustup`, `cargo`, and `git` on Linux (`rustup`
reads [`rust-toolchain.toml`](../rust-toolchain.toml) automatically). These
commands match [`.github/workflows/cargo.yml`](../.github/workflows/cargo.yml):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked --bin cn --bin conaryd --bin conary-mcp

sudo install -m 0755 target/release/cn /usr/local/bin/cn
sudo install -m 0755 target/release/conaryd /usr/local/bin/conaryd
sudo install -m 0755 target/release/conary-mcp /usr/local/bin/conary-mcp
```

## Running `conaryd` with systemd

A non-NixOS sample unit lives at
[`packaging/systemd/conaryd.service`](../packaging/systemd/conaryd.service)
(and inside each release tarball at `systemd/conaryd.service`). It uses
standard Linux paths managed by systemd:

- Binaries: `/usr/local/bin/conaryd` (and `/usr/local/bin/cn` when flow
  workers are enabled)
- Ledger state: `/var/lib/conary` (`StateDirectory=conary`, mode `0700`)
- Unix socket: `/run/conary/conaryd.sock` (`RuntimeDirectory=conary`, mode
  `0750`)
- Configuration and secrets: `/etc/conary` (`ConfigurationDirectory=conary`,
  mode `0750`)
- Optional environment file: `/etc/conary/conaryd.env`

### Initial setup

1. Create the dedicated service user and group:

   ```sh
   sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin conary
   ```

   Add any local users who should query `/run/conary/conaryd.sock` (for
   example via `conary-mcp`) to the `conary` group. Anyone who can open the
   socket can read the full ledger ([`docs/conaryd.md`](conaryd.md)).

2. Prepare `/var/lib/conary` and initialize the ledger as the `conary` user
   (`conaryd` refuses to start until `cn init` has initialized and anchored
   the ledger):

   ```sh
   sudo install -d -o conary -g conary -m 0700 /var/lib/conary
   sudo -u conary env \
     CONARY_LEDGER=/var/lib/conary \
     CONARY_ANCHOR_REPO=owner/ledger \
     CONARY_TOKEN_COMMAND=/usr/local/bin/conary-token \
     /usr/local/bin/cn init
   ```

   You can pass `GH_TOKEN` instead of `CONARY_TOKEN_COMMAND` if a single token
   covers every repository, though GitHub App installation tokens are scoped
   per owner and typically use `CONARY_TOKEN_COMMAND`
   ([`tokens.rs`](../crates/conary-github/src/tokens.rs)).

3. Install and start the systemd unit:

   ```sh
   sudo install -m 0644 packaging/systemd/conaryd.service /etc/systemd/system/conaryd.service
   sudo systemctl daemon-reload
   sudo systemctl enable --now conaryd.service
   sudo systemctl status conaryd.service
   ```

4. Optional listeners (`--events-config` and `--gate-reuse-config`):

   By default the sample unit serves only the read-only ledger Unix socket at
   `/run/conary/conaryd.sock`. To enable verified webhook ingress and flow
   passes ([`docs/conaryd.md#verified-flow-events`](conaryd.md#verified-flow-events))
   or gate-reuse lookups ([`docs/gate-reuse.md`](gate-reuse.md)):

   - Place your machine-owned JSON configs and webhook secret in
     `/etc/conary` owned by `conary:conary` with restrictive permissions
     (`0600` for secrets). Keep flow mailbox `data` under `/var/lib/conary`
     (for example `/var/lib/conary/data`), which `ProtectSystem=strict` keeps
     writable via `StateDirectory=conary`.
   - Either set `CONARY_EVENTS_CONFIG=/etc/conary/events.json` and/or
     `CONARY_GATE_REUSE_CONFIG=/etc/conary/gate-reuse.json` in
     `/etc/conary/conaryd.env`, or override `ExecStart` with
     `sudo systemctl edit conaryd.service` as shown in the comments in
     [`packaging/systemd/conaryd.service`](../packaging/systemd/conaryd.service).
   - If you run custom flow plugins or token helpers that require a JIT
     runtime (such as Node.js), relax `MemoryDenyWriteExecute=no` in a
     drop-in override.
   - Terminate TLS in front of the HTTP listener(s).

## GitHub App prerequisites

### Required repository permissions

Every GitHub API call Conary makes lives in
[`crates/conary-github/src/api.rs`](../crates/conary-github/src/api.rs) and
[`crates/conary-github/src/api/`](../crates/conary-github/src/api/). Grant
only the permissions needed for the modes you run:

| GitHub App repository permission | Anchor repo (`CONARY_ANCHOR_REPO`) | Ledger + git notes (`cn init`/`sync`/`ingest`/`backfill`/`class`) | Gate reuse (`conaryd --gate-reuse-config`) | `cn undo` | Merge train (`cn train`) | Full `conaryd` landing flow (`--events-config` / `cn flow`) |
| --- | --- | --- | --- | --- | --- | --- |
| **Metadata** | Read | Read | Read | Read | Read | Read |
| **Contents** | Read & write *(Read only for `cn verify`)* | Read & write *(writes `refs/notes/conary`; Read only if not writing notes)* | Read | Read & write | Read & write | Read & write |
| **Pull requests** | — | Read | Read | Read & write | Read & write | Read & write |
| **Issues** | — | Read | — | Read | Read | Read & write |
| **Checks** | — | Read | Read | Read | Read | Read & write |
| **Commit statuses** | — | Read | — | Read | Read & write | Read & write |
| **Actions** | — | — | Read | — | Read & write *(Read only if never rerunning runs)* | Read & write |
| **Workflows** | — | — | — | Read & write *(only if reverting `.github/workflows/*`)* | Read & write *(only if merging PRs that edit `.github/workflows/*`)* | Read & write *(only if landing/updating PRs that edit `.github/workflows/*`)* |

Notes:

- **Metadata: Read** is granted automatically to all GitHub Apps.
- **Anchor repository isolation:** `CONARY_ANCHOR_REPO` only stores
  `refs/conary/anchor` and needs no Pull requests, Issues, Checks, Commit
  statuses, or Actions permissions unless it is also a tracked repository.
- **CI attestation signing:** `cn attest --sign` runs inside CI jobs using
  GitHub Actions OIDC (`id-token: write`), not the GitHub App token. See
  [`docs/ci-attestation.md`](ci-attestation.md).

### Endpoint-to-permission audit

Each permission in the matrix above traces directly to endpoints implemented
in `crates/conary-github/src/api/`:

- **Ledger anchor** ([`api/writes.rs`](../crates/conary-github/src/api/writes.rs),
  `Anchors`):
  - **Contents: Read** — `GET /repos/{repo}/git/ref/{ref}`,
    `GET /repos/{repo}/git/commits/{sha}`
  - **Contents: Read & write** — `POST /repos/{repo}/git/trees`,
    `POST /repos/{repo}/git/commits`, `POST /repos/{repo}/git/refs`,
    `PATCH /repos/{repo}/git/refs/{ref}` (creates and fast-forwards
    `refs/conary/anchor`)
- **Git notes publishing** ([`api/writes.rs`](../crates/conary-github/src/api/writes.rs),
  `Notes`):
  - **Contents: Read** — `GET /repos/{repo}/git/ref/{ref}`,
    `GET /repos/{repo}/git/commits/{sha}`,
    `GET /repos/{repo}/git/trees/{sha}?recursive=1`
  - **Contents: Read & write** — `POST /repos/{repo}/git/trees`,
    `POST /repos/{repo}/git/commits`, `POST /repos/{repo}/git/refs`,
    `PATCH /repos/{repo}/git/refs/{ref}` (creates and updates
    `refs/notes/conary`)
- **Ledger ingest, sync, backfill, and classification**
  ([`api.rs`](../crates/conary-github/src/api.rs),
  [`api/history.rs`](../crates/conary-github/src/api/history.rs), `Source`):
  - **Pull requests: Read** — `GET /repos/{repo}/pulls`,
    `GET /repos/{repo}/pulls/{number}`,
    `GET /repos/{repo}/pulls/{number}/reviews`,
    `GET /repos/{repo}/commits/{sha}/pulls`
  - **Issues: Read** — `GET /repos/{repo}/issues/{number}/comments`,
    `GET /repos/{repo}/issues/{number}/timeline`
  - **Contents: Read** — `GET /repos/{repo}/commits/{sha}`,
    `GET /repos/{repo}/compare/{base}...{head}` (JSON commit/file comparisons
    and `application/vnd.github.diff`)
  - **Checks: Read** — `GET /repos/{repo}/commits/{sha}/check-runs`,
    `GET /repos/{repo}/check-runs/{run}/annotations`
  - **Commit statuses: Read** — `GET /repos/{repo}/commits/{sha}/statuses`
- **CI attestation verification and gate reuse**
  ([`api/actions.rs`](../crates/conary-github/src/api/actions.rs), `Actions`;
  plus `Source` and `Branches::tree` in [`reuse.rs`](../crates/conary-github/src/reuse.rs)):
  - **Actions: Read** — `GET /repos/{repo}/actions/jobs/{job}`,
    `GET /repos/{repo}/actions/runs/{run}/artifacts`,
    `GET /repos/{repo}/actions/artifacts/{id}/zip`
  - Public OIDC signing keys (`https://token.actions.githubusercontent.com/.well-known/jwks`)
    are fetched unauthenticated.
- **Merge train** ([`api/writes.rs`](../crates/conary-github/src/api/writes.rs),
  `Branches` and `train::Forge`; plus `Source`, `Actions`, `Anchors`, `Notes`):
  - **Metadata: Read** — `GET /repos/{repo}` (default branch and allowed merge
    methods)
  - **Contents: Read & write** — `GET /repos/{repo}/git/ref/heads/{name}`,
    `GET /repos/{repo}/git/matching-refs/heads/{prefix}`,
    `GET /repos/{repo}/git/commits/{sha}`, `POST /repos/{repo}/git/refs`,
    `DELETE /repos/{repo}/git/refs/heads/{name}`, `POST /repos/{repo}/merges`
  - **Commit statuses: Read & write** — `POST /repos/{repo}/statuses/{sha}`
    (`conary/train`)
  - **Pull requests: Read & write** (and **Contents: Read & write**, plus
    **Workflows: Read & write** when landing changes under
    `.github/workflows/`) — `PUT /repos/{repo}/pulls/{pr}/merge`
  - **Actions: Read & write** — `GET /repos/{repo}/actions/runs/{run}`,
    `POST /repos/{repo}/actions/runs/{run}/rerun`
- **Undo** ([`api/writes.rs`](../crates/conary-github/src/api/writes.rs),
  `undo::Reverts` and `Branches`; plus `Source`, `Anchors`, `Notes`):
  - **Contents: Read & write** — `POST /repos/{repo}/git/commits`,
    `POST /repos/{repo}/git/refs`, `DELETE /repos/{repo}/git/refs/heads/{name}`,
    `POST /repos/{repo}/merges` (plus **Workflows: Read & write** if the
    reverted change touched `.github/workflows/`)
  - **Pull requests: Read & write** — `POST /repos/{repo}/pulls`,
    `GET /repos/{repo}/pulls?state=all&head=...`
- **Full `conaryd` landing flow**
  ([`api/flow.rs`](../crates/conary-github/src/api/flow.rs), `flow::Read`;
  [`api/flow_write.rs`](../crates/conary-github/src/api/flow_write.rs),
  `flow::Write`; plus `Source`, `Actions`, `Branches`, `train::Forge`):
  - **Contents: Read** — `GET /repos/{repo}/activity`
  - **Actions: Read & write** —
    `GET /repos/{repo}/actions/workflows/{workflow}/runs`,
    `GET /repos/{repo}/actions/runs/{run}`,
    `POST /repos/{repo}/actions/workflows/{workflow}/dispatches`,
    `POST /repos/{repo}/actions/runs/{run}/cancel`,
    `POST /repos/{repo}/actions/runs/{run}/rerun-failed-jobs`
  - **Checks: Read & write** —
    `POST /repos/{repo}/check-suites/{suite}/rerequest`
  - **Commit statuses: Read & write** — `POST /repos/{repo}/statuses/{head}`
    (`conary/settled`)
  - **Issues: Read & write** — `POST /repos/{repo}/issues/{pr}/labels`,
    `DELETE /repos/{repo}/issues/{pr}/labels/{label}`,
    `POST /repos/{repo}/issues/{pr}/comments`
  - **Pull requests: Read & write** (and **Contents: Read & write** for branch
    updates) — `PATCH /repos/{repo}/pulls/{pr}`,
    `PUT /repos/{repo}/pulls/{pr}/update-branch`, plus GraphQL
    (`POST /graphql`) reads (`repository`, `pullRequest`, `reviewThreads`,
    `timelineItems`, `checkSuites`) and mutations
    (`addPullRequestReviewThreadReply`, `resolveReviewThread`,
    `disablePullRequestAutoMerge`)

### Webhook events

For the exact list of webhook events to subscribe to when running
`conaryd --events-config` (exported programmatically by
`conary_github::events::hook_events()`), authentication requirements, and
receiver configuration, see
[`docs/conaryd.md#verified-flow-events`](conaryd.md#verified-flow-events).
