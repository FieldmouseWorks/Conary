# Fake base fixture for container integration

This is a **fake base for container integration only**. It is not a bootable
system and must never be used outside the test harness. It provides the `File`
capability `/sbin/init`, an executable `/sbin/init`, and the minimal boot layout
a publishable generation resolves for a staged boot root:

- `/sbin/init` -- the validated static binary copied from the host.
- `/boot/vmlinuz-conary-test` -- clearly fake bytes.
- `/boot/initramfs-conary-test.img` -- clearly fake bytes.
- `/boot/EFI/BOOT/BOOTX64.EFI` -- clearly fake bytes.

The kernel, initramfs, and EFI files **are not bootable**. They exist only so
generation publication can copy and hash an exact kernel/initramfs/EFI set; no
builder step parses or executes them in a container.

## Why it exists

Publishing a generation requires a self-contained root whose exact manifest has
an executable `/sbin/init` entrypoint
(`crates/conary-core/src/generation/builder/root_validation.rs`) and a complete
versioned kernel/initramfs/EFI asset set for the staged boot root
(`crates/conary-core/src/generation/builder/boot_assets.rs`, #1102). A real
machine adopts its live root or installs a base from a repository (#598),
neither of which the unbooted test container has. This fixture is test-only
scaffolding for that gap; it does not change the product rule.

## Staging

The payload is written into `stage/` at image staging time by
`apps/conary-test/src/container/image.rs`; the static binary comes from
`resolve_static_test_binary()`, and the boot files are fixed fake bytes written
exactly like `apps/conary/src/commands/test_helpers.rs::stage_test_boot_assets`.
Nothing under `stage/` or `output/` is committed.

The fixture is built only when a selected suite declares it in its typed
`requires_fixtures` list (`requires_fixtures = ["conary-test-base"]`); command
text never opts an image in. The base shares the shell resolver and ELF
validation: `resolve_static_test_binary()` requires a 64-bit little-endian
static `ET_EXEC`/static-PIE `ET_DYN` for the image architecture with an
executable `PT_LOAD` segment containing the entry point. The `/bin/sh` hook
probe is skipped for the base because `/sbin/init` never runs fixture hooks; when
a suite also installs `conary-test-shell`, the probed shared binary is staged as
both payloads.

## Installation

The harness is the only installer, so a suite never installs a declared fixture
from `suite.setup`. A suite opts in with
`requires_fixtures = ["conary-test-base"]`, and the harness installs the
declared fixture into the test container itself -- once per container and before
the first manifest that declares it runs -- with the same exec path and
arguments the old setup steps used:
`ccs install <fixture> --policy ${FIXTURE_CCS_POLICY} --sandbox always --yes`.
A failed installation aborts the run with a typed error naming the fixture, and
the manifest guard rejects a `suite.setup` step that installs any declared
fixture.
