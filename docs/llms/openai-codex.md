---
last_updated: 2026-09-23
revision: 3
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

State each durable rule once. Remove repeated instructions, examples, and tool
descriptions unless they encode a measured project requirement. Track both
startup context and growing conversation context; compare prompt changes on
representative Conary tasks rather than assuming more instruction is better.

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

Conary's standing requested model roles and delegation rules are in `AGENTS.md`.
When dispatching work, request the named model and effort through the available
harness controls; if the model is unavailable, report it without substitution.
Only report the selected model and effort when the harness exposes them. A
repository instruction edit cannot change or attest the running parent
session's model or effort. OpenAI's [Agents API session configuration guide](https://developers.openai.com/api/docs/guides/agents-api/configuration#update-settings-for-an-existing-session)
documents settings changes for API-managed sessions; Codex's host harness owns
the controls available in a particular session.

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
