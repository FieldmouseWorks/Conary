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

The source binary is chosen by `resolve_static_test_shell()`: when
`CONARY_TEST_STATIC_SHELL` is set, that exact executable path is used;
otherwise the first executable `busybox` on `PATH` is used. The suite's hook
execution is the functional proof that the shell runs.
