# conary-plugin

Wire contract types, validation helpers, execution provenance checks, and
conformance fixtures for Conary executable plugins (`conary-plugin v5`).

This crate has minimal dependencies (`serde` and `serde_json`, plus an optional
`schemars` feature for JSON Schema generation) and no dependency on
`conary-core` or `conary-github`.

## Versioning

`conary-plugin` versions track the published wire contract in
[`docs/plugins.md`](https://github.com/FieldmouseWorks/Conary/blob/main/docs/plugins.md):

- **Major (`N.0.0`)** matches the highest wire contract version `conary-plugin vN`
  (currently `5.x.y` for `conary-plugin v5`).
- **Minor (`N.x.0`)** adds backwards-compatible Rust helpers or types within
  `conary-plugin vN`.
- **Patch (`N.x.y`)** carries bug fixes that preserve both the wire format and
  the public Rust API.

## Usage

Read one JSON `Request` from stdin, handle `Task::Describe` and the work kinds
your plugin advertises, and write one JSON `Response` to stdout. See
`examples/plugin.rs` for a complete, deterministic plugin that passes
`cn plugin check`.

```rust,no_run
use conary_plugin::{Request, Response, version};
use std::io;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_reader(io::stdin())?;
    if version(&request.contract).is_none() {
        return Err("unsupported contract".into());
    }
    // Handle `request.task` and write a `Response` to stdout.
    let _ = std::marker::PhantomData::<Response>;
    Ok(())
}
```
