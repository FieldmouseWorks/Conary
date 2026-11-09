//! Build the shared example as a normal executable, not against guessed rlibs.
use conary_core::plugin::{Limits, Plugin};
use std::path::Path;
use std::time::Duration;

#[test]
fn the_flow_fixture_satisfies_the_published_contract() {
    let plugin = Plugin::describe(
        Path::new(env!("CARGO_BIN_EXE_conary-flow-test-plugin")),
        Limits {
            deadline: Duration::from_secs(10),
            response_bytes: 1_048_576,
        },
    )
    .unwrap();
    assert_eq!(plugin.check().unwrap().len(), 4);
}
