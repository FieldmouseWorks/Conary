# Notes

How the ledger reaches a clone that can't reach the ledger
([DESIGN section 7](DESIGN.md#7-interfaces)). Every landed change gets a note
on the trunk commit its landing made, on `refs/notes/conary` in the change's
own repository. `cn` writes them each time it records a repository's PRs
(`ingest`, `sync`, `backfill`), after the ledger's anchor moves, and only the
notes that are missing or out of date, in one commit on the ref.

## Reading them

`cn why` reads the ledger when it's set up on the box it runs on. Anywhere
else, a cloud session's fresh clone included, it fetches the notes from the
checkout's `origin` first and answers from them:

    cn why --human crates/conary-core/src/why.rs:1

That needs a full clone (blame and the walk to the head need the history) and
an `origin` naming the GitHub repository. `insteadOf` rewrites are fine: the
repository is read from the configured URL, and fetches go wherever git
sends them. By hand, `git fetch origin refs/notes/conary:refs/notes/conary`
and then `git log --notes=conary` shows each landed commit's note.

## v1

Each note is one JSON document. `conary` names the format:

```json
{
  "conary": "note v1",
  "change": "<change id>",
  "repo": { "owner": "FieldmouseWorks", "name": "burrow" },
  "pr": 93,
  "url": "https://github.com/FieldmouseWorks/burrow/pull/93",
  "summary": "<the PR's title>",
  "task": "<owner/name#N, or a thread's link>",
  "author": { "kind": "agent", "login": "<login>", "session": "<link>" },
  "landing": {
    "revision": "<revision id>",
    "before": "<trunk before>",
    "after": "<trunk after: the commit the note is on>",
    "by": { "kind": "status", "login": "<steward login>" },
    "at": 0
  },
  "evidence": [
    { "kind": "ci", "check": "flake", "result": "pass", "source": "<run URL>" }
  ]
}
```

`evidence` is the landed revision's, as the ledger records it: recorded, not
counted ([section 4.3](DESIGN.md#43-evidence)). The class joins the note when
the ledger has one (M4).

## What a note leaves out

- The PR's body. It can carry prompt text; the summary is the title alone.
- Session and thread links, unless the repository is one that
  `--session-links` (or `CONARY_SESSION_LINKS`) names. Some repos' rules keep
  those links out of their history (LoK-web's, for one), so a repository
  that isn't named gets notes that link nowhere but GitHub: no
  `author.session`, no `task` that is a link, and every other link, in the
  summary or the evidence, read as `[link left out]`.

So a note says nothing its repository doesn't already show to whoever can
read it.

## Trust

A note is a readout of the ledger, signed by nothing: anyone who can push to
the repository can write one. `cn why` from notes says so, and nothing that
decides (landing, undo, counting evidence) reads them; those read the signed
ledger.
