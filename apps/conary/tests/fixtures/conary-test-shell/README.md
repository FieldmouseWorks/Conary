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

The fixture is built only when a selected suite's setup installs
`${FIXTURE_SHELL_CCS}`; images for suites that install neither this nor the
`/sbin/init` provider never consult a host binary. The source binary is chosen
by the shared `resolve_static_test_shell()` resolver: when
`CONARY_TEST_STATIC_SHELL` is set, that exact path is used; otherwise the first
`busybox` on `PATH` that validates is used. Validation requires a 64-bit
little-endian ELF for the image architecture, of type `ET_EXEC` or static-PIE
`ET_DYN`, with no `PT_INTERP` and no `DT_NEEDED` entries, because it runs in an
otherwise empty selected-root chroot. The suite's hook execution is the
functional proof that the shell runs.
