---
last_updated: 2026-09-25
revision: 3
summary: The advanced packaging and platform command surface hidden from default CLI help, and the commands authorized without --yes
---

# Advanced Commands

The default `conary --help` shows the daily-driver surface: install, remove,
update, search, list, autoremove, pin/unpin, try, system, repo, config,
distro, and self-update.

The advanced packaging and platform surface is hidden from default help but
fully supported at its existing paths. List it any time with:

```bash
conary --help-advanced
```

The listing is rendered from the CLI's own command tree, so this page does
not duplicate it; run the command for the current surface. Broad areas:

- **Packaging and recipes:** `cook`, `new`, `publish`, `recipe-audit`, `ccs`
- **System modeling and composition:** `model`, `collection`, `groups`,
  `derive`, `derivation`, `profile`, `cache`
- **Provenance and trust:** `provenance`, `capability`, `trust`,
  `verify-derivation`, `sbom`, `canonical`, `registry`
- **Platform and distribution:** `bootstrap`, `federation`, `export`,
  `query`, `automation`, `mcp`

Every command keeps `conary <command> --help`.

## Commands That Do Not Require `--yes`

Commands that change packages, files, generation state, or native authority
require `--yes` unless run with `--dry-run`. The command-risk policy in
`apps/conary/src/command_risk.rs` authorizes these without it:

- `conary self-update`, `conary try keep`, `conary try rollback`, and
  `conary try --activate`, which carry apply intent themselves. The
  `self-update --check` and verify forms are read-only.
- `conary system adopt` in its `--system`, package, and `--refresh` forms,
  which change Conary's tracking records rather than host files.
- The root-only `conary system adopt --refresh --quiet --from-sync-hook`
  native package-manager hook.
- The hidden boot-time `conary system generation activate` continuation,
  authorized by the selected generation artifact and kernel command line.
- The local-state class (`conary repo`, `conary pin`, `conary unpin`,
  `conary system init`, and similar), which is not gated even where a member
  writes host files, as `conary config restore` does.
