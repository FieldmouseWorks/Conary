# Hermetic `/sbin/init` provider fixture

This package provides the `File` capability `/sbin/init` and ships an executable
at `sbin/init`. Publishing a generation requires a self-contained root whose
exact manifest has an executable `/sbin/init` entrypoint
(`crates/conary-core/src/generation/builder/root_validation.rs`, #1102).
Integration suites that must publish generations install this fixture so the
install path can complete publication without a booted system.

The product path is adopting the live root or installing a base from a Remi
repository (#598), neither of which the unbooted test container has. This
fixture is test-only scaffolding for that gap; it does not change the product
rule that a published generation must own a real init entrypoint.

The payload is a statically linked binary copied into `stage/sbin/init` at image
staging time by `apps/conary-test/src/container/image.rs`; it is not committed
to the repository. `stage/` and `output/` are ignored.

The fixture is built only when a selected suite's setup installs
`${FIXTURE_INIT_CCS}`; images for suites that install neither this nor the
`/bin/sh` provider never consult a host binary. It shares the validated static
binary resolution with the `/bin/sh` provider: the source is chosen by
`resolve_static_test_shell()`, which validates a 64-bit little-endian static
`ET_EXEC`/static-PIE `ET_DYN` for the image architecture. Generation validation
checks that `/sbin/init` is an executable regular file, so the suite's
publication and package payload are the functional proof.
