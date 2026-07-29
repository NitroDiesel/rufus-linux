//! Bootloader selection and FreeDOS / UEFI:NTFS asset planning.
//!
//! Actual installation is performed by the privileged helper using distribution
//! tools or verified packaged assets. This crate only decides *what* is needed.

use rufus_core::plan::{BootMode, FileSystem, ImageSourceKind, WriteMode};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BootError {
    #[error("boot configuration unavailable: {0}")]
    Unavailable(String),
    #[error("incompatible boot options: {0}")]
    Incompatible(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootloaderKind {
    None,
    Syslinux4,
    Syslinux6,
    Grub2,
    Grub4Dos,
    FreeDos,
    UefiNtfs,
    WindowsBootmgr,
    IsoHybridNative,
}

impl BootloaderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Syslinux4 => "Syslinux 4",
            Self::Syslinux6 => "Syslinux 6",
            Self::Grub2 => "GRUB 2",
            Self::Grub4Dos => "GRUB4DOS",
            Self::FreeDos => "FreeDOS",
            Self::UefiNtfs => "UEFI:NTFS",
            Self::WindowsBootmgr => "Windows Boot Manager",
            Self::IsoHybridNative => "ISOHybrid (native)",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootPlan {
    pub bios_loader: BootloaderKind,
    pub uefi_loader: BootloaderKind,
    pub requires_uefi_ntfs: bool,
    pub requires_esp: bool,
    pub notes: Vec<String>,
}

/// Decide bootloader components for a write operation.
pub fn plan_boot(
    write_mode: WriteMode,
    boot_mode: BootMode,
    filesystem: FileSystem,
    image_kind: ImageSourceKind,
    windows_installer: bool,
) -> Result<BootPlan, BootError> {
    if boot_mode == BootMode::NonBootable {
        return Ok(BootPlan {
            bios_loader: BootloaderKind::None,
            uefi_loader: BootloaderKind::None,
            requires_uefi_ntfs: false,
            requires_esp: false,
            notes: vec!["Non-bootable media selected.".into()],
        });
    }

    match write_mode {
        WriteMode::DdImage => Ok(BootPlan {
            bios_loader: BootloaderKind::IsoHybridNative,
            uefi_loader: BootloaderKind::IsoHybridNative,
            requires_uefi_ntfs: false,
            requires_esp: false,
            notes: vec![
                "DD mode preserves the image's own boot structures; no extra bootloader install."
                    .into(),
            ],
        }),
        WriteMode::FreeDos => {
            if !matches!(filesystem, FileSystem::Fat | FileSystem::Fat32) {
                return Err(BootError::Incompatible(
                    "FreeDOS requires a FAT/FAT32 filesystem".into(),
                ));
            }
            if matches!(boot_mode, BootMode::Uefi) {
                return Err(BootError::Incompatible("FreeDOS is BIOS-only".into()));
            }
            Ok(BootPlan {
                bios_loader: BootloaderKind::FreeDos,
                uefi_loader: BootloaderKind::None,
                requires_uefi_ntfs: false,
                requires_esp: false,
                notes: vec![
                    "FreeDOS system files will be installed from redistributable assets.".into(),
                ],
            })
        }
        WriteMode::WindowsToGo | WriteMode::IsoFileCopy if windows_installer => {
            let needs_uefi_ntfs = matches!(filesystem, FileSystem::Ntfs)
                && matches!(boot_mode, BootMode::Uefi | BootMode::Dual);
            Ok(BootPlan {
                bios_loader: if matches!(boot_mode, BootMode::Bios | BootMode::Dual) {
                    BootloaderKind::WindowsBootmgr
                } else {
                    BootloaderKind::None
                },
                uefi_loader: if needs_uefi_ntfs {
                    BootloaderKind::UefiNtfs
                } else if matches!(boot_mode, BootMode::Uefi | BootMode::Dual) {
                    BootloaderKind::WindowsBootmgr
                } else {
                    BootloaderKind::None
                },
                requires_uefi_ntfs: needs_uefi_ntfs,
                requires_esp: matches!(boot_mode, BootMode::Uefi | BootMode::Dual),
                notes: {
                    let mut n =
                        vec!["Windows installer boot files will be arranged on the target.".into()];
                    if needs_uefi_ntfs {
                        n.push("UEFI:NTFS payload required for UEFI boot from NTFS.".into());
                    }
                    n
                },
            })
        }
        WriteMode::IsoFileCopy => {
            let use_grub = matches!(
                image_kind,
                ImageSourceKind::Iso | ImageSourceKind::IsoHybrid
            );
            Ok(BootPlan {
                bios_loader: if matches!(boot_mode, BootMode::Bios | BootMode::Dual) {
                    if use_grub {
                        BootloaderKind::Grub2
                    } else {
                        BootloaderKind::Syslinux6
                    }
                } else {
                    BootloaderKind::None
                },
                uefi_loader: if matches!(boot_mode, BootMode::Uefi | BootMode::Dual) {
                    BootloaderKind::Grub2
                } else {
                    BootloaderKind::None
                },
                requires_uefi_ntfs: false,
                requires_esp: matches!(boot_mode, BootMode::Uefi | BootMode::Dual)
                    && matches!(
                        filesystem,
                        FileSystem::Ntfs | FileSystem::ExFat | FileSystem::Ext4
                    ),
                notes: vec![
                    "ISO file-copy mode installs a distribution-compatible bootloader.".into(),
                ],
            })
        }
        WriteMode::FormatOnly => Ok(BootPlan {
            bios_loader: BootloaderKind::None,
            uefi_loader: BootloaderKind::None,
            requires_uefi_ntfs: false,
            requires_esp: false,
            notes: vec!["Format-only: no bootloader installation.".into()],
        }),
        WriteMode::WindowsToGo => Err(BootError::Unavailable(
            "Windows To Go requires a Windows image source".into(),
        )),
    }
}

/// External tool / package needed to install a bootloader kind.
pub fn provider_hint(kind: BootloaderKind) -> Option<&'static str> {
    match kind {
        BootloaderKind::None | BootloaderKind::IsoHybridNative | BootloaderKind::WindowsBootmgr => {
            None
        }
        BootloaderKind::Syslinux4 | BootloaderKind::Syslinux6 => {
            Some("syslinux package (syslinux/extlinux)")
        }
        BootloaderKind::Grub2 => Some("grub package (grub-install / grub2-install)"),
        BootloaderKind::Grub4Dos => Some("packaged GRUB4DOS assets"),
        BootloaderKind::FreeDos => Some("packaged FreeDOS assets under assets/freedos"),
        BootloaderKind::UefiNtfs => Some("signed UEFI:NTFS payload under assets/uefi"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dd_mode_uses_native_image_boot() {
        let plan = plan_boot(
            WriteMode::DdImage,
            BootMode::Dual,
            FileSystem::Fat32,
            ImageSourceKind::IsoHybrid,
            false,
        )
        .expect("DD image boot plan");
        assert_eq!(plan.bios_loader, BootloaderKind::IsoHybridNative);
    }

    #[test]
    fn freedos_rejects_uefi() {
        let err = plan_boot(
            WriteMode::FreeDos,
            BootMode::Uefi,
            FileSystem::Fat32,
            ImageSourceKind::None,
            false,
        );
        assert!(err.is_err());
    }

    #[test]
    fn windows_ntfs_requests_uefi_ntfs() {
        let plan = plan_boot(
            WriteMode::IsoFileCopy,
            BootMode::Uefi,
            FileSystem::Ntfs,
            ImageSourceKind::Iso,
            true,
        )
        .expect("Windows NTFS boot plan");
        assert!(plan.requires_uefi_ntfs);
        assert_eq!(plan.uefi_loader, BootloaderKind::UefiNtfs);
    }
}
