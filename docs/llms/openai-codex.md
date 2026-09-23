---
last_updated: 2026-09-23
revision: 5
summary: OpenAI-specific notes for Codex context, session controls, and the Conary agent workflow
---

# OpenAI/Codex Notes

Conary's shared assistant contract lives in `AGENTS.md` and
`docs/llms/README.md`; this page records OpenAI- and Codex-specific controls.

Verify time-sensitive behavior against current official documentation:

- [Codex `AGENTS.md` discovery](https://learn.chatgpt.com/docs/agent-configuration/agents-md)
- [Codex best practices](https://learn.chatgpt.com/guides/best-practices)
- [Prompting](https://learn.chatgpt.com/docs/prompting)
- [Current OpenAI model guidance](https://developers.openai.com/api/docs/guides/latest-model)

## Keep Startup Context Lean

Codex loads `AGENTS.md` automatically. Do not tell it to reread that file or
preload the full ownership map. Route a task through `agent-context`, then open
only the selected card's sources and canonical docs.

Check global instructions and the project chain through the session's working
directory; inspect applicable subtree instructions before editing below it.
Global `AGENTS.override.md` takes precedence over `AGENTS.md`; <!-- repo-path: hypothetical -->
per project directory, Codex selects at most one file in this order:
`AGENTS.override.md`, `AGENTS.md`, then configured fallback filenames. The <!-- repo-path: hypothetical -->
default aggregate instruction limit is 32 KiB. See the official
[instruction discovery rules](https://learn.chatgpt.com/docs/agent-configuration/agents-md)
for host configuration details. Keep host-local paths and instruction contents
in private notes or receipts, never tracked guidance.

After setting up or changing instructions, verify the effective chain in a
fresh read-only session when the host permits it. If it does not, report that
verification as unavailable or unverified instead of assuming it succeeded.

State each durable rule once. Remove repeated instructions, examples, and tool
descriptions unless they encode a measured project requirement. Track both
startup context and growing conversation context; compare prompt changes on
representative Conary tasks rather than assuming more instruction is better.
A linked Markdown page is not loaded automatically merely because it is linked;
open `docs/llms/agent-workflow.md` when the task needs its execution procedure.

## Prompt The Outcome

A strong task packet names:

- the goal and relevant owner or path;
- constraints and authority boundaries;
- what completion means;
- the exact evidence required.

State whether the request is to inspect, plan, implement, review, debug, or
verify. Keep dynamic branch state, failing commands, run IDs, and one-off notes
in the current issue, PR, or prompt rather than durable docs. Ask for findings,
decisions, concise rationale, and observed verification—not hidden
chain-of-thought.

Conary's requested model roles and delegation choices are in `AGENTS.md`; they
are project workflow choices, not universal rankings or availability
guarantees. Request model and effort through the available host controls, and
preserve the host's permission boundaries. If a model is unavailable, report
that without substitution. Report selected model and effort only when the
harness exposes them: editing repository instructions cannot change or attest
the running parent session. OpenAI's [Agents API session configuration guide](https://developers.openai.com/api/docs/guides/agents-api/configuration#update-settings-for-an-existing-session)
documents settings changes for API-managed sessions; the host owns controls
available in each session.

If using an optional supervised helper, including DeepSeek, follow the
[shared agent workflow](agent-workflow.md) and the active session's
authorization. Conary's default roles do not require contributors to use
OpenAI tools.

## Long Work

Use a plan for genuinely multi-step work and keep it synchronized with actual
progress. Preserve completed actions, exact identities, tool results, active
assumptions, unresolved blockers, and the next concrete step across compaction
or handoff. Retain bulky evidence in its owning log or artifact and return the
smallest complete summary.

Keep tool behavior in tool schemas, skills, plugins, or harness configuration.
There is no active OpenAI API prompt harness in this repository; product
automation UI under `conary-core` is not a model prompt layer.
