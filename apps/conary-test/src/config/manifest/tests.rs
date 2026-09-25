// apps/conary-test/src/config/manifest/tests.rs

#![cfg(test)]

use super::*;

#[test]
fn qemu_boot_defaults_to_qcow2_image_format() {
    let manifest: TestManifest = toml::from_str(
        r#"
            [suite]
            name = "QEMU format"
            phase = 3

            [[test]]
            id = "TQEMU01"
            name = "default format"
            description = "default qemu image format"
            timeout = 1

            [[test.step]]
            [test.step.qemu_boot]
            image = "minimal-boot-v3"
            commands = ["true"]
            "#,
    )
    .unwrap();

    let qemu = manifest.test[0].step[0].qemu_boot.as_ref().unwrap();
    assert_eq!(qemu.image_format, QemuImageFormat::Qcow2);
}

#[test]
fn qemu_boot_parses_iso_image_format() {
    let manifest: TestManifest = toml::from_str(
        r#"
            [suite]
            name = "QEMU format"
            phase = 3

            [[test]]
            id = "TQEMU02"
            name = "iso format"
            description = "iso qemu image format"
            timeout = 1

            [[test.step]]
            [test.step.qemu_boot]
            image = "local-generation-iso"
            image_format = "iso"
            commands = ["true"]
            "#,
    )
    .unwrap();

    let qemu = manifest.test[0].step[0].qemu_boot.as_ref().unwrap();
    assert_eq!(qemu.image_format, QemuImageFormat::Iso);
}

/// The canonical install slot for one variant, written as an exhaustive match.
///
/// A new [`StaticFixture`] variant fails to compile here until its author names
/// its slot and whether it closes the order. That is what lets
/// [`static_fixture_all_lists_every_variant_exactly_once`] prove `ALL` is
/// complete, duplicate-free, and ordered.
fn canonical_slot(fixture: StaticFixture) -> (usize, bool) {
    match fixture {
        StaticFixture::Shell => (0, true),
    }
}

#[test]
fn static_fixture_all_lists_every_variant_exactly_once() {
    for (index, fixture) in StaticFixture::ALL.iter().enumerate() {
        let (slot, is_last) = canonical_slot(*fixture);
        assert_eq!(
            slot,
            index,
            "{} must occupy slot {slot} in StaticFixture::ALL",
            fixture.declaration()
        );
        assert_eq!(
            is_last,
            index + 1 == StaticFixture::ALL.len(),
            "{} must agree with StaticFixture::ALL about the final slot",
            fixture.declaration()
        );
    }

    // An empty ALL would skip the loop above, so require the final variant to be
    // present explicitly.
    let final_is_last = match StaticFixture::ALL.last() {
        Some(fixture) => canonical_slot(*fixture).1,
        None => false,
    };
    assert!(
        final_is_last,
        "StaticFixture::ALL must end with the final variant"
    );
}
