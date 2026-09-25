# Hermetic `/bin/sh` provider fixture

This package provides the `File` capability `/bin/sh` and ships a static shell
at `bin/sh`. It exists so integration suites can install the
`conary-test-fixture` package, whose post-install and pre-remove hooks name
`/bin/sh` as their interpreter (#1080). The product path -- installing a shell
from a Remi repository -- depends on production universe activation (#598),
which the test environment does not have.

The payload is a statically linked shell copied into `stage/bin/sh` at image
staging time by `apps/conary-test/src/container/image.rs`; it is not committed
to the repository. `stage/` and `output/` are ignored.

A suite opts in only through its typed `requires_fixtures` list
(`requires_fixtures = ["conary-test-shell"]`); command text never opts an image
in, so quoting or rewriting an install command cannot silently drop the shell.
The harness then installs the declared fixture into the test container itself --
once per container and before the first manifest that declares it runs -- with
the same exec path and arguments the old setup steps used:
`ccs install <fixture> --policy ${FIXTURE_CCS_POLICY} --sandbox always --yes`.
A suite must not install a declared fixture from `suite.setup`, and a failed
installation aborts the run with a typed error naming the fixture. Images for
other suites never consult a host shell.

The source binary is chosen by `resolve_static_test_shell()`: when
`CONARY_TEST_STATIC_SHELL` is set, that exact path is used; otherwise the first
`busybox` on `PATH` that validates is used. Validation requires a 64-bit
little-endian ELF for the image architecture, of type `ET_EXEC` or static-PIE
`ET_DYN`, with no `PT_INTERP` and no `DT_NEEDED` entries, an executable
`PT_LOAD` segment with file content, and an entry point inside it. The candidate
is then functionally probed: it is run with `argv[0] = "sh"` in a cleared
environment whose `PATH` is empty and asked to `touch` and `rm` a probe file, so
a shell without standalone `touch`/`rm` applets is refused before staging.
