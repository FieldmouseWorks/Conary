# Conary design

Draft 0.1 · 2026-10-22 · MIT licensed

Conary is version control built for coding agents rather than humans. Its unit of work is a **changeset** that knows why it exists, carries its own proof, and can always be rolled back. Changesets land on their own when the proof is green. Every state the repository has ever been in stays one command away.

Git assumes one human typing slowly. It has a staging area, hand-named branches, a hash that changes on every rebase, and a merge button at the end. Conary assumes hundreds of agents producing changes in minutes. In that world, undo is cheap and attention is expensive.

## 1. Goals and non-goals

**Goals**

1. **Agent-first interface.** Every operation is an API call with structured input and output. The CLI is a thin client over the same API.
2. **Nothing is ever lost.** Every mutation is an entry in an append-only operation log. Undo is a new entry, never a deletion.
3. **The changeset is the unit.** A changeset has a stable identity from birth, no matter how often it is amended or moved.
4. **Landing without humans.** Changes that can be undone land when their evidence is green. Only irreversible actions wait for a person.
5. **Hundreds of concurrent lanes.** Parallel work is cheap, has no names to manage, and rarely conflicts.
6. **Evidence is bound to the change.** Test results attach to the changeset and the base they were checked against, and survive a rebase when the base didn't change what matters.
7. **Git interop.** Import any git history, and export to git so GitHub, CI and other people keep working.
8. **Fast.** Snapshot, query and land operations finish in milliseconds. Only verification takes real time.

**Non-goals for v1**

- Federated multi-user collaboration. GitHub stays the place other people meet the code.
- Storing large binaries or build output in history. Those go to a separate content-addressed store with retention.
- Windows support.
- Replacing the build or test system. Conary records evidence. Runners produce it.

## 2. Core model

The model has six parts.

```
Operation log  ──records every change to──▶  View
View           = { trunk head, lane heads, changeset → latest revision }
Lane           = an ordered stack of changesets for one task
Changeset      = stable id + revisions + intent + class + evidence + status
Revision       = one immutable snapshot of a changeset (tree before, tree after)
Object store   = content-addressed blobs and trees (BLAKE3)
```

### 2.1 Changeset

A changeset is created when a task starts and keeps its ID until it lands or is abandoned.

| Field | Meaning |
| --- | --- |
| `id` | 128-bit random ID, shown as a short prefix (like `kq7tmz`). Never changes. |
| `revisions` | Ordered list of immutable revisions. Every amend, rebase or resolution adds one. |
| `lane` | The lane it belongs to. |
| `intent` | Why it exists: task ID, a one-line summary, and a hash of the originating prompt or issue, with the full text in the object store. |
| `author` | Who made it: actor kind (agent or human), session ID, tool, and model. Signed by the actor's key. |
| `class` | Reversibility class. **R0**: undo in seconds, nothing leaves the box. **R1**: leaves the box but is easy to revert. **R2**: irreversible, needs a person. Set by policy from paths and actions, never by the author alone. |
| `evidence` | References to evidence records (section 5). |
| `status` | `draft`, `proposed`, `queued`, `landed`, `reverted`, `abandoned`. |
| `links` | Optional: depends-on, supersedes, reverts. |

Each revision stores the tree before, the tree after, and a semantic patch between them (section 4). Its revision ID is the hash of those contents, so identical work deduplicates.

### 2.2 Lane

A lane is a stack of changesets for one piece of work, usually one agent task. The control plane creates a lane when it starts a task and closes it when the task's changesets land or die. Lanes have IDs, not names, but can carry an optional label for humans. Trunk is a lane with special landing rules.

Thousands of open lanes is normal and costs only metadata.

### 2.3 Working copy

The working copy is always the tip revision of one changeset, as in Jujutsu. There is no staging area and no "uncommitted changes" state. The working copy is snapshotted:

- after every agent tool call, via a hook from the control plane, and
- on file-change events from fanotify or inotify, coalesced at 1 second.

Each snapshot that changes the tree adds a revision and an operation. Snapshots coalesce in the log view but stay individually addressable.

### 2.4 Operation log

Every mutation appends one operation:

```
Op {
  id:        hash of (parent ops, actor, kind, payload, timestamp)
  parents:   [op id]          // more than one when concurrent ops merge
  actor:     actor id, signed
  kind:      snapshot | new | amend | move | propose | land | revert | undo | resolve | import | export | ...
  payload:   kind-specific
  view:      hash of the resulting View
  at:        timestamp
}
```

The log is hash-chained and signed, so it is tamper-evident. The current View can always be rebuilt by replaying the log, and is cached.

Undo comes in four forms, all implemented as new operations:

| Command | What it does |
| --- | --- |
| `undo <op>` | Restore the View from before that op. |
| `undo --since 10m` | Restore the View as it was 10 minutes ago. |
| `undo --actor <session> [--since T]` | Revert everything one agent session did. Uses inverse ops, and any conflicts with later work become conflict objects for an agent to resolve. |
| `at <time or op> <command>` | Run any read-only query against a past View without changing anything. |

### 2.5 Conflicts

Conflicts are data, not a stop sign. A tree can contain a conflict object that records the base and each side. Conflicts can be stored and moved like any other state. A conflicted changeset can stay in its lane indefinitely, but can't land on trunk. Resolving one is a `resolve` operation that adds a revision, and the control plane hands the job to an agent by default.

## 3. Lanes, trunk and landing

```
lane A:  a1 ─ a2 ─ a3 ─┐
lane B:  b1 ───────────┼─▶ queue ─▶ verify candidate trunk ─▶ land ─▶ trunk
lane C:  c1 ─ c2 ──────┘                    │
                                            └─ fail ─▶ bisect ─▶ bounce culprit back to its lane with evidence
```

Trunk moves only through `land` operations. The landing policy depends on class:

| Class | Landing rule |
| --- | --- |
| R0 | Lands automatically when required evidence on the candidate trunk tree is green. |
| R1 | Lands the same way, then a post-land check runs. If it fails, Conary creates the revert changeset and lands it. |
| R2 | Waits in the decision inbox for a person to approve. The evidence must still be green. |

The queue builds speculative batches. A failing batch is bisected, the culprit changeset goes back to its lane with the failure evidence attached, and its agent is notified. The rest of the batch lands.

A revert is an ordinary changeset whose patch is the inverse, with intent `revert <id>: <reason>` and a link to the evidence that triggered it. Nothing is rewritten.

## 4. Semantic patches and commutation

Line-based diffs are why git conflicts so often under parallel agents. Two agents editing different functions in the same file shouldn't need a rebase.

**Representation.** A revision's patch is stored per file:

- **Structured files** (any language with a tree-sitter grammar) are diffed as edit operations on syntax nodes, addressed by a stable path such as `module › impl Foo › fn parse` plus the node kind. Moves and renames are recognised as such.
- **Everything else**, or a file that fails to parse, falls back to a text diff.

**Commutation.** Two changesets commute when their touched node sets are disjoint and they don't both add the same identifier in the same scope. Commuting changesets apply in either order and give the same tree, so they land without a rebase. Overlapping changes go through structured three-way merge (Mergiraf as the engine), then text merge, and anything still unresolved becomes a conflict object.

**The honest caveat.** Two changes can be syntactically independent and still break each other, for example by changing a function and its caller's assumptions. That is why the queue always verifies the final candidate trunk tree, never just each changeset alone. Commutation removes the rebase chore, not the verification.

## 5. Evidence

Evidence is a record that a check ran against a specific tree in a specific environment:

```
Evidence {
  changeset, revision
  base_tree:     tree hash it was checked against
  env:           environment digest (Nix derivation hash of the toolchain)
  check:         name and command
  result:        pass | fail | error, exit code, duration
  log:           object hash of output
  runner:        id of the runner that produced it, signed
  negative:      result of the same check on the parent tree (for new tests)
}
```

Three rules make evidence trustworthy:

1. **Agents don't grade their own work.** Evidence counts only when it is signed by a runner outside the agent's sandbox. An agent can run tests for its own feedback, but those results don't land anything.
2. **New tests must fail on the old code.** A check marked as a new test must have a `negative` result of fail on the parent tree. A test that passes either way proves nothing.
3. **Reuse beats re-running.** Evidence for a revision stays valid on a new base when the patch applies to it unchanged and none of the files in the check's footprint changed. The footprint starts as the touched files plus their direct importers. Test-impact data refines it later. Otherwise the check runs again.

## 6. Interfaces

One core, four ways in:

| Interface | Used by | Notes |
| --- | --- | --- |
| `conary-core` (Rust library) | conaryd, tests, tools | All logic lives here. |
| `conaryd` (daemon, JSON-RPC over a Unix socket) | Control plane, CLI | One per host. Serialises writes, serves reads concurrently, streams events. |
| MCP server | Agents | The same calls as tools, with schemas. |
| `cn` CLI | Humans and scripts | JSON output by default, `--human` for people. |

The core calls are:

```
lane.open(task, base) / lane.close
change.new(lane, intent) / change.describe / change.split / change.squash
change.snapshot                       // usually automatic
change.propose(class?)                // class is computed; the author can only raise it
evidence.add / evidence.query
land(changes | lane) / queue.status
undo(op | since | actor) / at(time | op, query)
log.query(path?, symbol?, actor?, intent~text?, since?, class?, status?)
why(path, symbol | line)              // the changeset, its intent and evidence that last touched it
diff(a, b, semantic=true)
conflicts.list / resolve(change, tree)
events.subscribe(land | bounce | revert | conflict | decision)
```

`why` is the call that git can't answer well. Asking why `parse_config` behaves a certain way returns the changeset that last changed it, the task and prompt that caused it, and the evidence it landed with.

## 7. Storage

- **Objects:** BLAKE3 content-addressed blobs and trees in a local store, packed in the background. Large blobs over a threshold are chunked.
- **Metadata:** SQLite in WAL mode for changesets, revisions, lanes, evidence and indexes (path, symbol, actor, intent text via FTS5).
- **Operation log:** an append-only, hash-chained file plus an index in SQLite. The file is the source of truth, and the index can be rebuilt.
- **Out of history:** build output, VM images and evidence logs past retention live in a separate store with TTL-based garbage collection.
- **On remi:** the control plane materialises workspaces as ZFS clones. Conary tracks content, not filesystems, so it works the same on a laptop without ZFS.

**Performance targets** (100k-file repo, warm cache):

| Operation | Target |
| --- | --- |
| Snapshot with 10 changed files | under 50 ms |
| `log.query` / `why` | under 50 ms |
| `land` bookkeeping, excluding verification | under 100 ms |
| `undo --since` | under 200 ms |
| Open a new lane | under 20 ms |

## 8. Git interop

- **Import:** each git commit becomes a landed changeset. Intent comes from the commit message, the author is mapped, and the SHA is kept in a mapping table. Merge commits become lanes landing.
- **Export:** trunk exports to git deterministically, so the same Conary state always gives the same SHAs. Each exported commit carries trailers: `Conary-Change: <id>`, `Conary-Intent: <summary>`, `Conary-Class: R0`. Lanes can optionally export as branches.
- **Inbound:** commits that arrive from GitHub (other people, bots) import as changesets and go through the same landing rules.
- **Colocated mode:** a `.git` directory sits next to `.conary` during the transition, so editors, CI and existing scripts keep working.

## 9. Build strategy

Don't write a storage engine first. The new ideas are intent, classes, evidence, landing, commutation and per-actor undo. None of them needs a new object store on day one.

| Stage | What | Why |
| --- | --- | --- |
| A | Build on `jj-lib`. A jj change ID is the changeset ID. Jujutsu supplies the op log, working-copy snapshots and conflict objects. `conaryd` adds lanes, intent, classes, evidence, the queue and per-actor undo, stored in its own SQLite next to the repo. | Reaches a usable tool in weeks instead of months, on a proven core. |
| B | Replace the merge path with semantic patches and commutation. Mergiraf is the structured merge engine. | This is the biggest win for parallel agents, and it fits behind jj's merge interface. |
| C | Go standalone (own object store, BLAKE3, own op log format) only if jj's model blocks something specific. Per-actor undo and evidence-keyed reuse are the likely triggers. | Avoids a rewrite until there's a concrete reason for one. |

## 10. Security

- Every operation is signed by its actor. Agent sandboxes get an ephemeral key from the secret broker, scoped to one lane.
- An agent's key can only write to its own lane. Landing on trunk is done by `conaryd` under its own key, after the policy check.
- Evidence counts only when it is signed by a runner key outside the sandbox.
- The hash-chained op log means "what did this agent do" has a trustworthy answer, and `undo --actor` can act on it.

## 11. Milestones

| Milestone | Delivers | Exit check |
| --- | --- | --- |
| M0 | This doc, a Rust workspace skeleton, CI, MIT license | `cargo test` passes in CI |
| M1 | `conaryd` on jj-lib: lanes, changesets with intent and class, op-log queries, `undo --since`, `undo --actor`, `why` | Conary's own repo is developed in Conary |
| M2 | Evidence store, signed runners, negative-control rule, local landing queue with bisect and auto-revert, MCP server | An agent lands an R0 change on Conary with no human involved, and a deliberately broken change gets reverted automatically |
| M3 | Semantic patches and commutation for Rust, Python and TypeScript. Deterministic git export to GitHub | Two agents edit different functions in one file at the same time and both land with no rebase |
| M4 | wend and LoK-web move onto Conary, and the lok-* scripts are retired | A full working day on both projects where Peter only answers R2 asks |
| M5 | Decide whether to go standalone (stage C) based on M1–M4 pain | A written decision |

## 12. Open questions

1. **Evidence footprint precision.** Touched files plus importers is safe but re-runs too much. When is test-impact data good enough to trust?
2. **Changeset granularity.** One changeset per task is the default. Should agents split by logical step automatically?
3. **Offline laptops.** Jujutsu already merges concurrent operation logs. Is that enough for a laptop that edits offline and syncs with remi later?
4. **Class policy format.** Is it a file of path globs and action rules, or code? It probably starts as TOML and grows a small rule language.
5. **CLI name.** `cn` is short and doesn't clash with common Unix tools. Check before M1.
