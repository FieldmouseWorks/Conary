# M6: the remaining repository cutovers (S2)

S1 is live: remi lands through conaryd since 2026-11-20
([#314](https://github.com/FieldmouseWorks/Conary/issues/314) has the
record). This page plans S2 in [m6-order.md](m6-order.md#switches): every
other repository the steward runs moves to conaryd, a group at a time,
burrow last. Each group is one task, filed as [Filing](#filing) says. What
a switch, a settle period and a shadow are stays with m6-order.md. How a
handover works stays with burrow's
[docs/landing.md](https://github.com/FieldmouseWorks/burrow/blob/main/docs/landing.md).
The owner's goal: off the steward as fast as it's safe.

Written 2026-11-20 against Conary's main at `350d051`, remi's at `a416a33`
and burrow's at `143cd3e`. The issues linked here own the live state.

## Inventory

Read from what remi runs, not from memory:
- the steward's last pass (`/var/lib/remi-update/autoland/digest-repos.json`,
  2026-11-20 07:07Z);
- its deployed configuration (`autoland.json`, the `repos` overrides);
- `settings show`;
- burrow-landing's record (`landing/owners.json`);
- the class policy remi deploys;
- each repository's workflows, rulesets and last two weeks of PRs on
  GitHub.

`settings show` said:
- automated work allowed for burrow, Conary and remi;
- PSIV paused by name, and every other repository paused as unlisted;
- remi landed by conaryd (generation 3), every other repository by the
  steward.

| Repository | Visibility | Work | Its gate runs on | Proofs | Repo R2 paths | PRs merged 11-06..20 | Group |
| --- | --- | --- | --- | --- | --- | --- | --- |
| FieldmouseWorks/remi | private | allowed | `fresh-remi`, PRs and train candidates | none | none | | S1, done |
| FieldmouseWorks/Conary | public | allowed | hosted | none | none | 150 | [S2a](#s2a-conary) |
| FieldmouseWorks/.github | public | paused | hosted | none | none | 52 | [S2b](#s2b-public-paused-repositories) |
| FieldmouseWorks/website | public | paused | hosted | none | none | 31 | S2b |
| TusanHomichi/TusanHomichi | public | paused | hosted | none | none | 28 | S2b |
| TusanHomichi/the-mortal-estate | public | paused | hosted, in legs | none | none | 35 | S2b |
| FieldmouseWorks/consolebook | public | paused | hosted, beside `pr-gate.yml` | none | `docs/decisions/`, `PRINCIPLES.md` | 22 | S2b |
| FieldmouseWorks/callcraft | public | paused | its own `ci.yml`, not burrow's gate | none | none | 3 | S2b |
| TusanHomichi/MiraCraft | private | paused | `miracraft`, a kept self-hosted runner | none | none | 31 | [S2c](#s2c-private-paused-repositories) |
| TusanHomichi/career | private | paused | `career`, a kept self-hosted runner | none | none | 96 | S2c |
| TusanHomichi/LoK-web | private | paused | `lok-flake`, a kept self-hosted runner | none | none | 60 | S2c |
| TusanHomichi/foresight-revival | private | paused | nothing: no workflows | none | none | 4 | S2c |
| TusanHomichi/PSIV | public | paused | hosted | `gate` (`proofRuns`, one slot) | none | 200 or more | [S2d](#s2d-psiv) |
| FieldmouseWorks/wend | public | paused | its own `gate.yml` | `remi-proof` (`proofJobs`, a model) | none | 12 | [S2e](#s2e-wend) |
| FieldmouseWorks/burrow | public | allowed | hosted, and `agents` here | `agents` (`proofBuilds`, one slot) | none | 200 or more | [S2f](#s2f-burrow) |

- **Shared R2 paths:** every repository also has the policy's, for
  CODEOWNERS, secrets and sops files.
- **Train-candidate CI:** no repository but remi runs its gate on train
  candidates today. Each one's `flake.yml` runs on pushes to `main` only.
- **"200 or more":** the query's own cap.

**Reconciled:**
- **The count:** the steward's pass holds 15 repositories, remi among them,
  so 14 remain: 2 allowed and 12 paused. The 11-20 relay counted 14 with
  remi (11 paused). The one it missed is a repository created this month
  (callcraft on 11-15 or foresight-revival on 11-04), both in the pass and
  both here.
- **switchyard:** `TusanHomichi/switchyard` has a dashboard file from
  11-18, but no longer exists on GitHub and isn't in the pass. There's
  nothing to cut over.
- **One group each:** every repository in the pass has exactly one group.
- **No two controllers:** none has two at once, since a handover's two
  steps keep the steward and conaryd apart (landing.md).

## Every repository lands through its train

The flow counts a required check only as an attestation of the commit
that lands, on that commit's own tree (`flow/evidence.rs`'s `lookup`,
[flow.md](flow.md#producer-evidence)). A PR's own run can't give one. A
`pull_request` job attests GitHub's test merge, a revision subject, never
the head's tree ([ci-attestation.md](ci-attestation.md)). A dry `cn flow
pass` of Conary can't show it yet, since no open PR there reached a
landing step on 11-20, so S2a's shadow is where it's first seen live.

The train is what makes counting evidence: its candidate is a pushed
commit, the tree that lands, and its push run attests it, as remi's does.
So every repository cuts over with its train on. That also fixes the class
of red trunk two green PRs make (fence 10), and gives the shared R2 paths a
way to land, since the flow lands an R2 change only through the train.

For each repository, that means:
- `train` on, with `evidence.require` naming its gate's checks and
  `negative` its `check / negative` (that one is a PR run, and counts as
  `conary negative`);
- its `flake.yml` running the gate on pushes to `conary/train/**`, on a
  runner whose results count: GitHub-hosted, or one started for the job;
- and where a ruleset is possible, `conary/train` required. burrow's
  baseline adds that for a repository with its train on.

Turning a train on under the steward is a deploy, and the steward steps a
train too. So a handback leaves the train on, and the landing rollback
stays one setting.

## What blocks every group

**X1, one flow configuration per repository.** conaryd reads one flow
configuration for every repository it lands (`flow_config` in its events
file). burrow refuses a `burrow.landing.conaryd` list whose repositories
render different ones (`modules/autoland/landing.nix`). Conary's
hosted-runner evidence can't share remi's `fresh / check`, so no group can
switch without per-repository configuration. burrow's shadow already
renders one configuration per repository (`autoland-shadow/OWNER/NAME.json`),
so a new group's shadow runs today. Two tasks:

- **X1a (Conary):**
  - conaryd's events file gives each repository its own flow
    configuration, each loaded and checked as `cn flow settings` does.
  - A pass of a repository reads only its own.
  - One file for all stays valid.
  - Its sim test runs two repositories with different `evidence` in one
    conaryd.
- **X1b (burrow, after X1a is live on remi):**
  - renders each repository's configuration into conaryd's events file;
  - drops the one-configuration assertion;
  - renders `evidence.fresh` per repository, not as remi's `fresh-remi-`
    for every repository.
  - Its check is a rendered Conary and remi configuration that
    `cn flow settings` loads.

**X2, nothing moves a trunk conaryd holds without its permission.** The
flow already lands a Renovate PR it has settled through Renovate's own
automerge, as a delegated settlement ([flow.md](flow.md#effects-and-retries)).
In a train repository it goes through the train. Two gaps remain:

- **The private TusanHomichi repositories have no ruleset.** GitHub needs
  Pro for one there. So nothing requires `review-settled` or
  `conary/train`, and Renovate merges its own PRs on green checks. It did
  in LoK-web and career on 11-20.
- **People and sessions merge by hand.** In 4 of Conary's last 40 PRs, a
  person merged (admins can merge past the ruleset). The house's guidance
  still tells a session it may merge a judged PR itself.

**X2 (burrow)** closes both:
- Renovate doesn't merge its own PR in a repository conaryd holds, read
  from burrow-landing's record at each run, so a handback needs no deploy
  (`off-switch-independent`).
- The house's guidance stops telling a session it may merge in one.
- Its check is a Renovate run against such a repository that merges
  nothing, failing before the change.

It's a prerequisite of S2c and of the hand-merge part of every group's
ownership check.

**Pauses.** A paused repository under conaryd lands nothing, Renovate's
PRs included: that's what a pause means to the flow. That's a change from
today, where Renovate merges its own in paused repositories. Also,
`settings land` is refused for a repository conaryd holds. So a paused
repository's one-off landing is an `allow`, until `scope-below-a-repo`
gives a narrower pause. A CI change a paused repository needs (its train
trigger, below) lands while the steward still holds it. Either the
operator allows the repository for it, or `settings land` lands its
settled PR.

## Plugins, and the flow properties that wait

- **Fix, judge and proof plugins** come with #314's second remi PR
  (burrow#1274, burrow#1186, the producer keys). The rendered `plugins`
  are global, so every repository conaryd holds gets them once they're
  live.
  - **Without them:** a PR that needs a fix or a judgment waits, named by
    `no_plugin`. That's safe, and the handback covers it, so they're no
    prerequisite for a group without proofs (S2a to S2c).
  - **Where a repository has a proof** (S2d to S2f), its plugin is a
    prerequisite. Without one, the proof's check never gets a counting
    result, and landing without it would weaken the gate.
  - **One switch at a time:** turning them on is a switch on remi, so it
    and a group's switch don't open together.
- **[FP9](https://github.com/FieldmouseWorks/Conary/issues/421)** (one
  repository's slow read delays only its own pass) is needed once conaryd
  passes more than remi and Conary. So it's a prerequisite of S2f, and of
  the first `allow` of a paused repository conaryd holds.
- **[FP11](https://github.com/FieldmouseWorks/Conary/issues/422)** (break a
  review loop) is a prerequisite of S2f: burrow is where loops run longest.
  Elsewhere a loop past the round cap waits for the judge plugin or the
  handback, as on remi.
- **FP5b to FP10** stay after S1, as the owner directed on 11-19. None is
  needed for a PR to land correctly.

## Every group, the same way

Each group's task does these, in order. Its section adds what's its own.

1. **Prerequisites live.**
   - Each named prerequisite has landed and is deployed on remi
     (fence 12).
   - The group's repositories have their train CI.
   - The group's values are in `burrow.autoland.repos`: `train`, the
     evidence and proofs. The steward's stale `requireCi = false` overrides
     go, since Conary, `.github` and TusanHomichi all have CI now.
2. **Train on, under the steward.** Same remi PR as the shadow (step 3)
   for a paused group. For an allowed repository it's its own remi PR, and
   a candidate lands through the steward's train before step 3.
3. **Shadow.**
   - One remi PR adds the group to `burrow.autoland.shadow`.
   - `cn flow settings` loads each rendered configuration.
4. **Judge the shadow** by m6-order.md's rule: [the shadow is a dry
   run](m6-order.md#the-shadow-is-a-dry-run-not-a-target).
   - **Judging:** every kind of disagreement in
     `autoland-shadow@<repo>`'s journal is judged against GitHub's state,
     as one of:
     - the flow's error: fixed in Conary, with a sim test from the PR, and
       live;
     - the steward's error: listed in
       [flow.md](flow.md#where-the-shadow-differs-from-the-steward);
     - timing.
   - **Passes when:** no kind is unjudged, none is the flow's error, and no
     shadow run has `error` set or `errors > 0`.
   - **Paused repositories:** the steward holds every PR there, so "the
     steward is paused" is an expected kind. What's judged is whether each
     flow decision is a step GitHub's state supports.
   - **Attestation:** a green PR waiting on `no attested check run` for
     its negative means the repository's attestations don't count. That's
     a CI prerequisite, not a flow bug.
   - **No fixed count of days.**
5. **List.** One remi PR adds the group to `burrow.landing.conaryd`.
6. **Switch.** `settings landing OWNER/REPO conaryd` for each repository,
   one at a time. Each handover finishes before the next starts.
7. **Ownership check**, for each repository, from the switch on:
   - **The record:** `settings show` names conaryd and the generation, and
     burrow-landing's record agrees.
   - **The steward lets go:** its journal logs that it released the
     repository, and it takes no landing action there afterwards: no step,
     job, proof, merge, train step, `review-settled` or stage label from
     its pass. Both controllers act as the steward App, so `mergedBy`
     can't tell them apart; the journals and the ledger can.
   - **The ledger:** every move of the default branch is a `land` of
     conaryd's from a train candidate, with no `outside` op.
   - **The events file:** conaryd's lists the repository exactly while its
     work is allowed.
8. **Rollback test**, on one repository of the group:
   - **The test:** `settings landing OWNER/REPO steward` finishes its
     handover, the steward's next pass takes the repository up, and
     `settings landing OWNER/REPO conaryd` gives it back. That's the test
     S1 ran at generations 2 and 3.
   - **The rollback:** the handback is the group's rollback throughout,
     with no deploy. Taking a repository off the list at a deploy hands it
     back too.
9. **Acceptance:** the settle period, which m6.md leaves to this plan.
   - **An allowed repository:** a PR lands through conaryd on attested
     candidate evidence, with the ownership check clean, then two days of
     normal work with nothing traced to the switch.
   - **A paused group:** every handover finished both ways, and a day with
     no `outside` op.
   - **Records:** the task records each in its issue, with the ops and
     generations.

**When a paused group may run.** A paused group's switch decides nothing
differently until a repository is allowed, since conaryd isn't given a
paused repository. So it may run while another group settles, but never
during another group's handover. The first `allow` of a paused repository
conaryd holds is the switch in m6-order.md's sense, one at a time:
- **Acceptance:** an allowed repository's, above. Whoever allows it records
  that first landing in the group's issue.
- **Whose call:** the allow is the operator's, as today.
- **Before it:** FP9.

## The groups

Each group runs after the one before it has settled, except where the
rule above lets a paused group run sooner.

### S2a: Conary

The owner wants a demo, and Conary is public, so its landings are
visible. It goes first, as soon as it's safe.

- **Opens when:** X1a and X1b are live, and Conary's `flake.yml` runs its
  gate on `conary/train/**` pushes (hosted runners: Conary is public, so
  they cost nothing).
- **Configuration:**
  - `train` on, `evidence.require = ["check / check"]`,
    `negative = "check / negative"`, `fresh` null.
  - `require_ci` on: the `requireCi = false` override goes.
- **Plugins:** none required. Until #314's second PR is live, a Conary PR
  that needs a fix or a judge waits, named by `no_plugin`. Its authoring
  session can still answer its findings.
  - **Handback trigger:** if a waiting PR blocks M6's own lane for more
    than a working day, hand Conary back until the plugins are live.
- **Bootstrapping:**
  - conaryd runs remi's pin of Conary, not Conary's main, so a bad Conary
    landing changes nothing live until remi's next bump.
  - That bump goes through remi's own train.
  - The handback needs neither.
- **Ownership:** a person merged 4 of Conary's last 40 PRs. The ruleset
  will require `conary/train`, which an admin can still merge past, so the
  guidance half of X2 is watched here first.

### S2b: public paused repositories

FieldmouseWorks/.github, FieldmouseWorks/website, TusanHomichi/TusanHomichi,
TusanHomichi/the-mortal-estate, FieldmouseWorks/consolebook and
FieldmouseWorks/callcraft.

- **Opens when:** S2a's switch has finished (its settle may still be
  running), X1b is live, and each repository's train CI has landed.
- **Configuration:** each repository's gate checks as its own runs name
  them (burrow's legged gate in the-mortal-estate), `check / negative`,
  `fresh` null, `train` on.
- **callcraft's CI:** callcraft needs more than a trigger. It needs
  burrow's gate (an attested `check / check` and negative), and the house
  workflow it doesn't have, as the repo baseline asks.
- **consolebook's R2 paths** (its decisions and `PRINCIPLES.md`) are the
  owner's to approve:
  - **How:** through the train, with the owner's marked review
    ([flow.md](flow.md#person-approval)). `pr-gate.yml`'s required checks
    count as green checks.
  - **Ownership:** the owner merges consolebook PRs by hand today
    (consolebook#232 to #234 on 11-20). Under conaryd that's an `outside`
    op, and the marked review replaces it.

### S2c: private paused repositories

TusanHomichi/MiraCraft, TusanHomichi/career, TusanHomichi/LoK-web and
TusanHomichi/foresight-revival.

- **Runners:** MiraCraft, career and LoK-web gate on self-hosted runners
  kept between jobs, whose attestations never count. foresight-revival has
  no CI at all. Each needs its gate on train candidates on a runner that
  counts:
  - **Started for the job:** a runner like `fresh-remi`, for which X1b
    gives each repository its own `fresh` prefix. This is the likely way,
    since these private gates need this machine.
  - **Or GitHub-hosted.**
  - **Also deferred heavy checks:** #298 to #300 stay with #292's list.
- **Opens when:** X2 is live (no ruleset holds Renovate or a merge here),
  S2b's handovers have finished, and the four repositories' CI has landed.
- **A late repository:** one whose CI hasn't landed when the others are
  ready stays with the steward, named in the group's issue. It isn't
  dropped.

### S2d: PSIV

- **Opens when:**
  - S2c's handovers have finished;
  - #314's second PR has made the proof plugin (task P) and its producer
    key live;
  - `evidence.producers` trusts that key for PSIV's `gate`;
  - PSIV's train CI has landed.
- **Configuration:**
  - **Proof:** PSIV's `proofRuns.gate` becomes the flow's `proofs` entry
    for `gate`, a `Run` recipe of the same command, one slot. The flow asks
    it of the head, and it counts only from the proof producer's service
    attestation of the head's tree.
  - **Train:** on, with `check / check` as S2b.
  - **Candidates:** if a candidate's own gate skips `gate`, it counts only
    under a signed machine proof of the candidate, which is
    [#218](https://github.com/FieldmouseWorks/Conary/issues/218). The
    shadow shows which.
- **Gate reuse:** #386 and #448 carry on as their own work.

### S2e: wend

[m6.md](m6.md#what-m6-doesnt-port) left this to #315: does wend's
model-run `remi-proof` become a program, or does wend stay on the steward?
**It becomes a program.**

- **Why:** staying on the steward would hold S4 for one repository, and the
  replays it asks for (`tools/vm.py`) are a program already.
- **What goes:** the model's part, deciding what the change needs and
  writing it up.
- **What replaces it:**
  - The proof runs the replays the head's paths call for, by a rule in
    wend's own tools.
  - Its receipt is the summary, as PSIV's gate does.
  - The `remi-verdict` workflow stops being the way a verdict is recorded
    for landing.

That's wend's own delivery: an issue in wend, beside an attested gate on
train candidates. Either `gate.yml` signs with `cn attest ci` as burrow's
gate does, with that workflow as wend's `signer`, or it adopts burrow's
gate.

- **Opens when:** S2d has settled and wend's change has landed.
- **Configuration:**
  - `proofs.remi-proof` as that recipe.
  - `job_timeout` as wend needs (the steward's 240 minutes are for its
    replays).

### S2f: burrow

burrow goes last, as fence 10 says.

- **Its train goes first, as its own task (S2f-train).** It's fence 10's
  first step, and the long pole. That task:
  - lifts the module's refusal of a train beside `proofBuilds`, in the
    same change;
  - counts the `agents` check on a candidate from a signed machine proof
    (#218, with P and its key), and closes remi#1077;
  - settles under the steward before anything below.
- **Opens when:**
  - S2e has settled, and S2f-train has settled;
  - #314's second PR is live with all of fix, judge and proof. burrow had
    the most open PRs (30 on 11-20), and most need a fix round;
  - FP9 and FP11 are live.
- **Configuration:**
  - `proofs.agents` as a `Build` recipe of `checks.x86_64-linux.agents`,
    one slot.
  - `leadRepos`' place as an explicit priority level
    (`explicit-priority` is built), not a queue of its own.
- **Settle:** two days of normal work. S3 waits on it.

## Order

The rule is m6-order.md's: build in parallel, switch one at a time.

1. X1a, then X1b. X2 and every repository's train CI run beside them.
   S2f-train can start once #218 and P allow it.
2. S2a.
3. S2b, from S2a's finished switch.
4. S2c, then S2d.
5. S2e.
6. S2f.

S4, the steward's Python deleted
([#316](https://github.com/FieldmouseWorks/Conary/issues/316)), waits on
every group, through S2f.

**S1 and the 11-23 target.**
- **S1 has settled:** #314's measured acceptance in m6.md (a remi PR
  landing through conaryd on attested candidate evidence) was met on 11-20
  by remi#1454.
- **What's open:** X1a, X1b and Conary's train trigger stand between that
  and S2a, so remi and Conary on conaryd by 11-23 is the reachable part.
- **What comes after:** the paused groups follow as their CI lands. burrow
  waits on its train and the plugins.

## Lessons

#315 and its tasks are held to the entries [m6.md](m6.md#lessons-each-task-is-held-to)
names. Each is already `designed` or `built`. How each group keeps to them:

- **`spenders-off-by-default`:** a group turns on only the plugins it
  names.
- **`off-switch-independent` and `live-settings`:** the handback is one
  setting, and X2 keeps Renovate's merges on the live owner.
- **`switches-show-their-effect`:** the ownership check.
- **`owner-only-for-doors` and `one-question-per-decision`:** an owner's
  approval is one marked review, for an R2 door only (consolebook).
- **`no-model-in-mechanical-step`:** S2e.
- **`heavy-work-waits-for-settled-inputs`:** #292's list, unchanged.
- **`same-inputs-same-verdict`:** evidence counts only on the tree that
  lands, a candidate's, and proofs only from service attestations of the
  tree they ran on (S2d to S2f).

## Filing

Once this plan lands, each task is filed at once, so none waits on
another's start:

- **In Conary:**
  - X1a, X1b and X2, as prerequisites; S2f-train; and each group, S2a to
    S2f.
  - X1b, X2 and S2f-train say their PRs are in burrow (or remi), and
    close them by full name (fence 8).
- **In each other repository that needs its train CI** (every one but remi
  and burrow, whose CI is S2f-train's), with wend's `remi-proof` program
  in wend's.
- **The title** starts with its ID.
- **The body:**
  - links its section here, names the lesson IDs it is held to, and says
    `Refs FieldmouseWorks/Conary#315` and `#256`;
  - carries its "opens when" as its fence;
  - says its PR carries the `first` label.
- **Labels:** each group stays `later` until its "opens when" holds, and
  the task whose finish makes it hold takes `later` off. So the next one
  starts on that event, not on someone noticing.

The tasks grant no authority beyond #256's.
