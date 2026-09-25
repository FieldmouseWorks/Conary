// apps/conary/src/ui/publication/tests.rs

#![cfg(test)]

use super::*;

#[test]
fn missing_init_command_failure_names_the_init_reason_without_a_retry_command() {
    assert_eq!(
        no_base_system_command_failure(
            "Generation publication",
            MissingBaseSystemPart::MissingInit,
        ),
        concat!(
            "Generation publication is still pending.\n",
            "Reason: selected root has no base system yet: no executable /sbin/init, so no generation can be published or booted\n",
            "The package change is committed and will publish once a base system with an executable /sbin/init is present.\n",
            "Adopt this machine's native system: conary system adopt --system\n",
            "Or install a base system that provides /sbin/init from a repository.",
        )
    );
}

#[test]
fn missing_boot_assets_recovery_failure_names_the_boot_asset_reason_without_a_retry_command() {
    assert_eq!(
        no_base_system_command_failure(
            "Generation publication recovery",
            MissingBaseSystemPart::MissingBootAssets,
        ),
        concat!(
            "Generation publication recovery is still pending.\n",
            "Reason: selected root has no base system yet: no kernel or boot assets, so no generation can be published or booted\n",
            "The package change is committed and will publish once a /boot/vmlinuz-<release> kernel and an EFI loader are present.\n",
            "Adopt this machine's native system: conary system adopt --system\n",
            "Or install a kernel package that provides /boot/vmlinuz-<release> and an EFI loader at /boot/EFI/BOOT/BOOTX64.EFI or systemd-boot's /usr/lib/systemd/boot/efi/systemd-bootx64.efi.",
        )
    );
}
