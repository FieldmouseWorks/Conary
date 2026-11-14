# The review loop's policy: one owner per decision

The review loop is reviewing a head, fixing or answering what the review
finds, counting rounds, and sending a head to a judge or to the owner. Its
policy has no single home today. It's split across the reviewer's brief,
the fix brief, the steward's step and several open burrow issues. Four
plugin tasks ([m6-plugins.md](m6-plugins.md)) are written in parallel, and
each one ports a piece of that policy. This page says, for each decision:

- the rule on burrow's main the day this was written: `fa6254e`, 2026-11-14;
- the one place that owns the decision once conaryd lands a repository;
- the open issue that changes the decision, if there is one.

The plugin tasks link this page and don't restate it.

This page changes no policy. Each open change lands where it's filed. Each
plugin ports what's on main on the day the plugin is written.

## Rules

- **One owner per decision.** After cutover each row has exactly one owner.
  Rounds have one counter, and it's the flow's. No plugin counts rounds,
  keeps per-PR round state, or words its brief by round.
- **One source per brief.** Each brief's text lives in burrow's
  `burrow-brief` crate ([task B1](m6-plugins.md#b1-briefs-one-source-for-the-review-fix-and-judge-briefs)).
  The steward reads it until [#316](https://github.com/FieldmouseWorks/Conary/issues/316)
  deletes the steward, and the plugin reads it too. If a plugin, a module or
  the steward carries its own copy of a brief's policy text, a reviewer
  should flag it.
- **Settings come from the settings source.** A number or switch belongs in
  burrow's shared settings source, and the flow configuration is generated
  from it ([flow.md](flow.md)). A plugin never reads a review-loop setting.

## The decisions

Paths in the "today" column are in burrow at `fa6254e`.

| Decision | The rule today | Owner after cutover | Open change |
| --- | --- | --- | --- |
| What a severity means | `crates/burrow-upkeep/src/brief.rs`: "P0" is unsafe or breaks things, "P1" is a real bug or a rule broken, "P2" is a smaller real defect. A reason in the description that doesn't hold is flagged P1. The plugin contract carries `p0` to `p3` ([plugins.md](plugins.md#work-requests-and-answers)), and the reviewer never sends `p3` | The review brief in `burrow-brief`, which the review plugin and the steward both read | [burrow#958](https://github.com/FieldmouseWorks/burrow/issues/958) |
| Which severities block a landing | Every finding blocks. The brief says the PR "merges itself once all of them find nothing on its head", and the steward asks the reviewers again until they do. Conary's flow does the same today: any finding goes to a fix | Conary's flow (`flow::review`, `flow::decide`) | burrow#958 changes how findings are graded, not this rule. See the open question below |
| What a fix does with a finding | `modules/autoland/tasks.py`, `MUST_FIX` and `FINDINGS`, the same from round one and past the limit. A concrete defect, or a break of a rule the repo's Code Review Rules list as P1, is fixed in full with a regression test. Everything else is answered with reasons and left unchanged. A P1 label alone doesn't make a finding one to fix | The fix brief in `burrow-brief`, which the fix plugin and `tasks.py` both read. Conary's flow posts the answers ([task C2](m6-plugins.md#c2-flow-give-each-finding-a-thread-and-post-a-fixs-answers-on-it)) | None. [burrow#949](https://github.com/FieldmouseWorks/burrow/issues/949) landed as [burrow#973](https://github.com/FieldmouseWorks/burrow/pull/973) |
| How many rounds with findings before the reviewers stop | `autoland.rounds`, default 5 (`crates/burrow-settings/src/schema.rs`), counted in `review_step.py`. Past the limit, `step.py` hands what's left to a fix job, and a judge reads the answers (`PAST_LIMIT`) | Conary's flow counts (`flow/reviews.rs`) and caps (`flow/review.rs`) at `settings.rounds`, whose value comes from the settings source | [burrow#748](https://github.com/FieldmouseWorks/burrow/issues/748) moves the limit into typed settings. [burrow#945](https://github.com/FieldmouseWorks/burrow/issues/945) records remi lowering it to 3, which is the owner's call |
| What else sends a head on (fix growth, an answered finding raised again) | Nothing yet | Conary's flow decides. The fix-growth share lives in the settings source. Porting the triggers into the flow is [#312](https://github.com/FieldmouseWorks/Conary/issues/312)'s to schedule | burrow#945. It hands the PR to the stuck role, not to the judge |
| Who judges | The contract's self-grading rule ([plugins.md](plugins.md#independence-and-execution-failures)): a fresh session, never the author's or a reviewer's. Today `tasks.py` starts a new job per head, base and description, and nothing compares the judge with the head's author | Conary's contract and runner (`conary_core::plugin`). The judge plugin starts a fresh session and reports it. The judging engine is the flow configuration's `execution.judge`, generated from the role map | None |
| What a loop that won't settle goes to before the owner | `step.py`: the third failure on a head is overdue. Then the watchdog kicks the pass, then a job, then the steward's stuck issue. A reviewer that fails three times goes on the digest | Conary's flow (strikes, overdue, owner listing). The stuck role's engine comes from the role map | [burrow#961](https://github.com/FieldmouseWorks/burrow/issues/961) for the expedited stages. burrow#945 for the stuck role |

## Open: does an answered P2 the reviewer still disputes block?

When a reviewer still disagrees with an answered P2, does that block a
landing, or does it only reach the judge? That's the owner's open question
on [burrow#939](https://github.com/FieldmouseWorks/burrow/issues/939), and
this plan doesn't answer it.

What happens on main today: the brief tells the reviewer not to raise an
answered finding again "unless the code still has the problem and the reply
is wrong". A finding raised again is a finding, so it blocks until the round
limit. After that, the judge reads the answers (`PAST_LIMIT`).

Whatever the answer, it is one switch, not a choice four plugins each make:

- The switch is a field in Conary's flow `Settings`, with its value from the
  settings source.
- Its default is today's behavior.
- It's implemented in the same change that ports burrow#945's repeat
  trigger into the flow, because both read the same finding and answer
  history.
- No plugin reads it. A reviewer reports what it finds, and a fixer fixes
  or answers.
