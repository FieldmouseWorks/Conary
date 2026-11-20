# M6: the remaining repository cutovers (S2)

S1 is live: remi lands through conaryd since 2026-11-20
([#314](https://github.com/FieldmouseWorks/Conary/issues/314) has the
record). This page plans S2 in [m6-order.md](m6-order.md#switches): every
other repository the steward runs moves to conaryd, a group at a time,
burrow last. Each group is one task, filed as [Filing](#filing) says. What
a switch, a settle period and a shadow are stays with m6-order.md; how a
handover works stays with burrow's
[docs/landing.md](https://github.com/FieldmouseWorks/burrow/blob/main/docs/landing.md).
The owner's goal: off the steward as fast as it's safe.

Written 2026-11-20 against Conary's main at `350d051`, remi's at `a416a33`
and burrow's at `ebfc26c`. The issues linked here own the live state.

## Inventory

From what remi runs, not from memory: the steward's last pass
(`/var/lib/remi-update/autoland/digest-repos.json`, 2026-11-20 07:07Z), its
deployed configuration (`autoland.json`, the `repos` overrides),
`settings show`, burrow-landing's record (`landing/owners.json`), the
class policy remi deploys, and each repository's workflows, rulesets and
last two weeks of PRs on GitHub.

`settings show` said: automated work allowed for burrow, Conary and remi;
PSIV paused by name; every other repository paused as unlisted; remi landed
by conaryd (generation 3), every other repository by the steward.

| Repository | Visibility | Work | Attested CI today | Proofs | Repo R2 paths | PRs merged 11-06..20 | Group |
| --- | --- | --- | --- | --- | --- | --- | --- |
| FieldmouseWorks/remi | private | allowed | `fresh / check`, `check / negative` (fresh-remi) | none | none | | S1, done |
| FieldmouseWorks/Conary | public | allowed | `check / check`, `check / negative` (hosted) | none | none | 150 | [S2a](#s2a-conary) |
| FieldmouseWorks/.github | public | paused | `check / check`, `check / negative` | none | none | 52 | [S2b](#s2b-paused-repositories-whose-ci-already-attests) |
| FieldmouseWorks/website | public | paused | `check / check`, `check / negative` | none | none | 31 | S2b |
| TusanHomichi/TusanHomichi | public | paused | `check / check`, `check / negative` | none | none | 28 | S2b |
| TusanHomichi/the-mortal-estate | public | paused | burrow's legged gate, `check / negative` | none | none | 35 | S2b |
| TusanHomichi/MiraCraft | private | paused | `check / check`, `check / negative` (hosted) | none | none | 31 | S2b |
| TusanHomichi/career | private | paused | `check / check`, `check / negative` (hosted) | none | none | 96 | S2b |
| TusanHomichi/LoK-web | private | paused | burrow's gate on `lok-flake`, which never counts | none | none | 60 | [S2c](#s2c-repositories-whose-ci-cant-attest-yet) |
| FieldmouseWorks/callcraft | public | paused | none: its own `ci.yml` | none | none | 3 | S2c |
| TusanHomichi/foresight-revival | private | paused | none: no workflows | none | none | 4 | S2c |
| TusanHomichi/PSIV | public | paused | `check / check`, `check / negative` | `gate` (`proofRuns`, one slot) | none | 200 | [S2d](#s2d-psiv) |
| FieldmouseWorks/wend | public | paused | none: its own `gate.yml` | `remi-proof` (`proofJobs`, a model) | none | 12 | [S2e](#s2e-wend) |
| FieldmouseWorks/consolebook | public | paused | `check / check`, `check / negative`, beside `pr-gate.yml` | none | `docs/decisions/`, `PRINCIPLES.md` | 22 | [S2f](#s2f-consolebook) |
| FieldmouseWorks/burrow | public | allowed | `check / check`, `check / negative`, `agents` here | `agents` (`proofBuilds`, one slot) | none | 200 | [S2g](#s2g-burrow) |

Every repository also has the policy's shared R2 paths (CODEOWNERS,
secrets, sops files). "Attested CI today" is what each repository's latest
PR ran; whether conaryd counts it is each group's shadow to show, below.

**Reconciled:** the steward's pass holds 15 repositories, remi among them,
so 14 remain: 2 allowed and 12 paused. The 11-20 relay counted 14 with
remi (11 paused); the one it missed is a repository created this month
(callcraft on 11-15 or foresight-revival on 11-04), both in the pass and
both here. `TusanHomichi/switchyard` has a dashboard file from 11-18 but no
longer exists on GitHub and isn't in the pass, so there's nothing to cut
over. Every repository in the pass has exactly one group, and none has two
controllers at once: a handover's two steps keep the steward and conaryd
apart (landing.md).

## What blocks every group

Three things hold for every repository, and each has one task.

**X1, one flow configuration per repository.** conaryd reads one flow
configuration for every repository it lands (`flow_config` in its events
file), and burrow refuses a `burrow.landing.conaryd` list whose
repositories render different ones (`modules/autoland/landing.nix`). No
group can switch without it: Conary checks on hosted runners with no train,
where remi requires `fresh / check` through its train. burrow's shadow
already renders a configuration per repository (`autoland-shadow/OWNER/NAME.json`),
so the shadow of a new group runs today, but it renders `evidence` only
for a repository with a train, so a repository without one shows
`no_evidence` at every landing step. Two tasks:

- **X1a (Conary):** conaryd's events file gives each repository its own
  flow configuration, every one loaded and checked as `cn flow settings`
  does, and a pass of a repository reads only its own. One file for all
  stays valid. Its sim test runs two repositories with different `train`
  and `evidence` in one conaryd.
- **X1b (burrow, after X1a is live on remi):** render each repository's
  `evidence` (`require`, `negative`, `signer`, `fresh`) whether or not it
  has a train, from `burrow.autoland.repos`, into conaryd's per-repository
  configuration and the shadow's alike, and drop the one-configuration
  assertion. Its check is a rendered Conary and remi configuration that
  `cn flow settings` loads.

**X2, nothing but conaryd moves a trunk it holds.** In the last 40 merged
Conary PRs, Renovate merged 8 of its own (it runs as the workers App with
`automergeType = "pr"`), and a person merged 4. In the paused repositories
Renovate still merges its own (LoK-web#896 and career#257 on 11-20). Under
conaryd each of those is an
`outside` op, a violation for the owner. And nothing on GitHub stops one in
the private TusanHomichi repositories: rulesets need GitHub Pro there, so
`review-settled` isn't required. remi doesn't show it because its train is
required. **X2 (burrow):** Renovate leaves the merge to whichever
controller holds the repository, read from burrow-landing's record at each
run, so a handback needs no deploy (`off-switch-independent`). The house's
guidance stops telling a session it may merge a PR itself in a repository
conaryd holds. Its check is a Renovate run against a repository conaryd
holds that merges nothing, failing before the change.

A paused repository under conaryd lands nothing, Renovate's PRs included.
That's what a pause means to the flow. It differs from today, where
Renovate merges its own in paused repositories, and `settings land` is
refused for a repository conaryd holds. A paused repository's one-off
landing is an `allow`, until `scope-below-a-repo` gives a narrower pause.

**S1 has settled.** That's m6-order.md's rule for S2.

## Plugins, and the flow properties that wait

- **Fix, judge and proof plugins** come with #314's second remi PR
  (burrow#1274, burrow#1186, the producer keys). The rendered `plugins`
  are global, so once they're live, every repository conaryd holds gets
  them. Without them, a PR that needs a fix or a judgment waits, and its
  switch names `no_plugin`. That's safe, and the handback covers it, so
  it's no prerequisite for a group with no proofs (S2a to S2c). A proof is
  a prerequisite where a repository has one (S2d, S2e, S2g): without its
  plugin, the proof's check never gets a counting result and nothing
  lands. Landing without it would weaken the gate.
- **[FP9](https://github.com/FieldmouseWorks/Conary/issues/421)** (one
  repository's slow read delays only its own pass) is needed once conaryd
  passes more than remi and Conary. So it's a prerequisite of S2g, and of
  the first `allow` of a paused repository conaryd holds.
- **[FP11](https://github.com/FieldmouseWorks/Conary/issues/422)** (break a
  review loop) is a prerequisite of S2g: burrow is where loops run
  longest. Elsewhere, a loop past the round cap waits for the judge plugin
  or the handback, as on remi.
- **FP5b to FP10** stay after S1, as the owner directed on 11-19. None of
  them is needed for a PR to land correctly.

## Every group, the same way

Each group's task does these, in order. Its section adds what's its own.

1. **Prerequisites live.** Each named prerequisite has landed and is
   deployed on remi (fence 12). The group's configuration values are in
   `burrow.autoland.repos`.
2. **Shadow.** One remi PR adds the group to `burrow.autoland.shadow`.
   `cn flow settings` loads each rendered configuration.
3. **Judge the shadow** by m6-order.md's rule: [the shadow is a dry
   run](m6-order.md#the-shadow-is-a-dry-run-not-a-target). Every kind of
   disagreement in `autoland-shadow@<repo>`'s journal is judged against
   GitHub's state as the flow's error (fixed in Conary, with a sim test from
   the PR, and live), the steward's error (listed in
   [flow.md](flow.md#where-the-shadow-differs-from-the-steward)) or timing.
   It passes when no kind is unjudged, none is the flow's error, and no
   shadow run has `error` set or `errors > 0`. In a paused repository the
   steward holds every PR, so "the steward is paused" is an expected kind.
   What's judged there is whether each flow decision is a step GitHub's
   state supports. In every group, no green head may wait on
   `no attested check run`: that's attested CI not counting, and it's a CI
   prerequisite, not a flow bug. There's no fixed count of days.
4. **List.** One remi PR adds the group to `burrow.landing.conaryd`.
5. **Switch:** `settings landing OWNER/REPO conaryd` for each repository,
   one at a time. Each handover finishes before the next starts.
6. **Ownership check**, for each repository, from the switch on:
   - `settings show` names conaryd and the generation, and burrow-landing's
     record agrees.
   - The steward's journal logs that it released the repository, and it
     takes no landing action there afterwards: no step, job, proof, merge,
     train step, `review-settled` or stage label from its pass. Both
     controllers act as the steward App, so `mergedBy` can't tell them
     apart; the journals and the ledger can.
   - Every move of the default branch is a `land` op of conaryd's in the
     ledger, and there are no `outside` ops (X2).
   - conaryd's events file lists the repository exactly while its work is
     allowed.
7. **Rollback test**, on one repository of the group:
   `settings landing OWNER/REPO steward` finishes its handover, the
   steward's next pass takes the repository up, and
   `settings landing OWNER/REPO conaryd` gives it back. That's the test S1
   ran at generations 2 and 3. The handback is the group's rollback
   throughout, with no deploy. Taking a repository off the list at a deploy
   hands it back too.
8. **Acceptance** (the settle period, which m6.md leaves to this plan):
   - **An allowed repository:** a PR lands through conaryd on attested
     evidence for the tree that lands, with the ownership check clean. Then
     two days of normal work with nothing traced to the switch.
   - **A paused group:** every handover finished both ways, and a day with
     no `outside` op.
   - **Recording:** the task records each in its issue with the ops and
     generations.

A paused group's switch decides nothing differently until a repository is
allowed: conaryd isn't given a paused repository. So it may run while
another group settles, but never during another group's handover. The
first `allow` of a paused repository conaryd holds is the switch in
m6-order.md's sense, one at a time. Its acceptance is the allowed
repository's above, and whoever allows it records that first landing in
the group's issue. That allow is the operator's call, as today, and FP9
comes first.

## The groups

Each group runs after the one before it has settled, except where the rule
above lets a paused group run sooner.

### S2a: Conary

The owner wants a demo, and Conary is public, so its landings are
visible. It goes first, as soon as it's safe.

- **Opens when:** S1 has settled, and X1a, X1b and X2 are live.
- **Configuration:**
  - `train` off, as today: changing how Conary lands at the same time
    as who lands it would make a red trunk hard to trace.
  - `evidence.require = ["check / check"]`, `negative = "check / negative"`,
    `fresh` null, since its CI runs on GitHub's hosted runners.
  - `require_ci` on: the steward's `requireCi = false` for Conary predates
    its CI, and the override goes.
- **Plugins:** none required. Until #314's second PR is live, a Conary PR
  that needs a fix or a judge waits, named by `no_plugin`. Its authoring
  session can still answer its findings.
  - **Handback trigger:** if a waiting PR blocks M6's own lane for more
    than a working day, hand Conary back until the plugins are live.
- **Shared R2 paths:** with the train off, a change to CODEOWNERS or a
  secret waits, named by `train_off`. One is landed by a handback, or by
  turning Conary's train on as its own switch.
- **Bootstrapping:** conaryd runs remi's pin of Conary, not Conary's main.
  So a bad Conary landing changes nothing live until remi's next bump, which
  goes through remi's own train, and the handback needs neither.
- **Ownership:** Conary has the most `outside` risk today (Renovate and
  people both merge), so X2's check is watched here first.

### S2b: paused repositories whose CI already attests

FieldmouseWorks/.github, FieldmouseWorks/website, TusanHomichi/TusanHomichi,
TusanHomichi/the-mortal-estate, TusanHomichi/MiraCraft and
TusanHomichi/career.

- **Opens when:** S2a's switch has finished (its settle may still be
  running), and X1b and X2 are live.
- **Configuration:**
  - The gate's attested checks as each repository's own runs name them:
    burrow's legged gate in the-mortal-estate.
  - `negative = "check / negative"`, `fresh` null, train off.
  - `.github`'s and TusanHomichi's `requireCi = false` overrides go:
    both have CI now.
- **CI check:** MiraCraft and career are private, and attest on hosted
  runners. The shadow must show their heads' attestations counting (no
  `no attested check run` on a green head). If either doesn't, it moves to
  S2c with a CI task in its repository, and this group goes without it,
  said in its issue.
- **Ownership:** MiraCraft and career have no ruleset (GitHub Free,
  private). So the ledger's `outside` count is the only guard, and X2 is
  what keeps it at zero.
- **Later:** #299 and #300 (deferred heavy checks) stay with #292's list
  and wait on this group, as m6.md says.

### S2c: repositories whose CI can't attest yet

TusanHomichi/LoK-web, FieldmouseWorks/callcraft and
TusanHomichi/foresight-revival. Each needs a CI change in its own
repository first. Those stay with the repository's own delivery, as an
issue there that this group's task files and waits on:

- **LoK-web:** its gate runs on `lok-flake`, a self-hosted runner kept
  between jobs, whose attestations never count. Its gate, or a
  `check / check` beside it, runs on a hosted runner or one started for
  the job. #298 (deferring its heavy gate) is separate and doesn't block
  this.
- **callcraft:** an attested gate and negative (burrow's flake-check, as
  the repo baseline asks), and the house workflow it doesn't have.
- **foresight-revival:** any CI at all: it has no workflows, so the flow
  can count nothing there.

**Opens when:** the three CI changes have landed, and S2b's handovers
have finished. A repository whose CI hasn't landed when the others are
ready waits on the steward, named in the group's issue. It isn't dropped.

### S2d: PSIV

- **Opens when:** S2c's handovers have finished, #314's second PR has made
  the proof plugin (task P) and its producer key live, and
  `evidence.producers` trusts the proof key for PSIV's `gate`.
- **Configuration:**
  - PSIV's `proofRuns.gate` becomes the flow's `proofs` entry for `gate`:
    a `Run` recipe of the same command, one slot (`proof_slots = 1`).
    It counts only from the proof producer's service attestation for the
    head's tree.
  - `check / check` and `check / negative` as S2b.
  - PSIV has no train. #218 (machine proofs in a train) isn't needed for
    it.
- **Shadow:** a disagreement about `gate` is judged like any other. A
  head the steward would merge on a `gate` status that the flow can't count
  is the steward's error, not the flow's.
- **Gate reuse:** #386 and #448 carry on as their own work. They don't
  block this.

### S2e: wend

[m6.md](m6.md#what-m6-doesnt-port) left this to #315: does wend's
model-run `remi-proof` become a program, or does wend stay on the steward?
**It becomes a program.** Staying on the steward would hold S4 for one
repository, and the replays it asks for (`tools/vm.py`) are a program
already. The model's part, deciding what the change needs and writing it
up, goes:

- The proof runs the replays the head's paths call for, by a rule in
  wend's own tools.
- Its receipt is the summary, as PSIV's gate does.
- The `remi-verdict` workflow stops being the way a verdict is recorded
  for landing.

That's wend's own delivery, an issue in wend this group's task files.

- **Opens when:** S2d has settled and wend's change has landed.
  - **wend's change:** `remi-proof` as a `Run` recipe, and an attested
    gate and negative. Either wend's `gate.yml` signs with `cn attest ci`
    as burrow's flake-check does, with that workflow as wend's `signer`, or
    it adopts flake-check.
- **Configuration:**
  - `proofs.remi-proof` as that recipe.
  - `job_timeout` as wend needs (the steward's 240 minutes are for its
    replays).

### S2f: consolebook

- **Train:** consolebook's own R2 paths (its decisions and
  `PRINCIPLES.md`) are the owner's to approve. In the flow an R2 change
  lands only through the train, with a person's marked review
  ([flow.md](flow.md#person-approval)). So consolebook turns its train on
  in this group's configuration.
  - Its candidates' CI runs on train branches, on hosted runners: a
    change to its `flake.yml`, its own delivery.
  - The other required checks (`fmt-clippy-test`, the two builds) count as
    green checks.
- **Opens when:** S2e's handovers have finished, X2 is live, and
  consolebook's train CI has landed.
- **Ownership:** the owner merges consolebook PRs by hand today
  (consolebook#232 to #234 on 11-20). Under conaryd that's an `outside`
  op. An owner's approval is the marked review instead.
- **Rollback:** the landing handback is one setting, and the steward runs
  a train too, so the train stays on after a handback. Turning the train
  off is a deploy, and isn't part of the rollback.

### S2g: burrow

burrow goes last, as fence 10 says.

- **Opens when:**
  - S2f has settled.
  - **burrow's train is on and has settled under the steward first.**
    That's fence 10's first step: the module's refusal of a train beside
    `proofBuilds` lifted in the same change, the `agents` check counted
    from a signed machine proof of the candidate (#218, P and its key),
    remi#1077.
  - #314's second PR is live, with all of fix, judge and proof. burrow
    carries the most open PRs (30 on 11-20), and most need a fix round.
  - FP9 and FP11 are live.
- **Configuration:**
  - `proofs.agents` as a `Build` recipe of `checks.x86_64-linux.agents`,
    one slot.
  - `leadRepos`' place as an explicit priority level (`explicit-priority`
    is built), not a queue of its own.
  - `require` as its candidates run it.
- **Settle:** two days of normal work. S3 waits on it.
- **Rollback:** the landing handback is one setting. The train stays on,
  since the steward runs it too. Undoing the train itself is a deploy, as
  it is today.

## Order

The rule is m6-order.md's: build in parallel, switch one at a time.

1. X1a, then X1b. X2 runs beside them.
2. S2a.
3. S2b, from S2a's finished switch.
4. The S2c and S2e repository CI tasks, and consolebook's train CI, run
   in their own repositories alongside, as soon as they're filed.
5. S2c, then S2d.
6. S2e, then S2f.
7. S2g.

S4, the steward's Python deleted
([#316](https://github.com/FieldmouseWorks/Conary/issues/316)), waits on
every group, through S2g.

## Lessons

#315 and its tasks are held to the entries [m6.md](m6.md#lessons-each-task-is-held-to)
names. Each is already `designed` or `built`. This plan says how each group
keeps to them:

- **`spenders-off-by-default`:** a group turns on only the plugins it
  names.
- **`off-switch-independent` and `live-settings`:** the handback is one
  setting, and X2 keeps Renovate's merges on the live owner.
- **`switches-show-their-effect`:** the ownership check.
- **`owner-only-for-doors` and `one-question-per-decision`:** an owner's
  approval is one marked review, for an R2 door only (S2f).
- **`no-model-in-mechanical-step`:** S2e.
- **`heavy-work-waits-for-settled-inputs`:** #292's list, unchanged.
- **`same-inputs-same-verdict`:** proofs count only from service
  attestations of the tree that lands (S2d, S2e, S2g).

## Filing

Once this plan lands, each task is filed as an issue in Conary:

- **Issues:** X1a, X1b and X2 are prerequisites. X1b and X2 say their PRs
  are in burrow, and close them by full name (fence 8). Each group from
  S2a to S2g is its own issue.
- **The issue's title** starts with its ID.
- **The issue's body:**
  - links its section here, names the lesson IDs it is held to, and says
    `Refs FieldmouseWorks/Conary#315` and `#256`;
  - carries its "opens when" as its fence;
  - says its PR carries the `first` label.
- **Labels:** each stays `later` until its "opens when" holds. The task
  whose finish makes it hold takes `later` off, so the next starts on
  that event, not on someone noticing.

The repository CI issues are filed in their own repositories by their
group's task when it starts. The tasks grant no authority beyond #256's.
