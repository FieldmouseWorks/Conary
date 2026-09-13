# Repeated Arch requirement declarations

`cargo-msrv-0.19.3-2.desc` is the byte-exact `cargo-msrv-0.19.3-2/desc`
member of the authenticated Arch `extra.db` used by the protected resolution
[survey 34170518393](https://github.com/FieldmouseWorks/Conary/actions/runs/34170518393).
[Issue #956](https://github.com/FieldmouseWorks/Conary/issues/956) owns its import failure.

- Profile revision: `021aa90aa6873ce44ef96ea623a527bf17b33206c1cb1f2cb59bc8e1267bd12a`
- Source database SHA-256: `ed7070126b979eeed0c287327829c36ae2e336ddd621870079f3286208fcf309`
- Descriptor SHA-256: `22d7551323b780ccbab3aa4b2cf9fb641f445281598503f30ff591f2caab599a`

The `%DEPENDS%` block declares `rustup` twice. The parser test preserves both
native declarations. Candidate-resolution tests separately prove that repeated
canonical groups retain package-fact multiplicity and yield the same exact
closure and unresolved-group evidence across worker counts. A direct projection
test additionally verifies six distinct stored group IDs across two packages
sharing the same group digest, with every occurrence mapped to its exact owner.
The fixture is metadata only; no package payload is installed.
