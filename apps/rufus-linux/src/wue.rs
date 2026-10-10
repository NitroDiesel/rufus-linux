//! The Windows User Experience dialog that upstream Rufus shows on Start for
//! Windows 10 and 11 installer media: which options apply to an image, the
//! remembered choices, and the request they become.

use rufus_helper_protocol::{RegionalSettings, WindowsCustomization};
use rufus_image::windows::WindowsImage;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WueOption {
    BypassRequirements,
    NoOnlineAccount,
    LocalAccount,
    Regional,
    NoDataCollection,
    SilentInstall,
    DisableBitlocker,
    QualityOfLife,
    ApplySkuSiPolicy,
    ForceSMode,
}

impl WueOption {
    /// Upstream's `MSG_3xx` captions and tooltips.
    pub fn text(self) -> (&'static str, &'static str) {
        match self {
            Self::BypassRequirements => (
                "Remove requirement for 4GB+ RAM, Secure Boot and TPM 2.0",
                "It is safe to leave this option enabled even if you have a TPM or more RAM, as this option only bypasses the setup requirements and does not actually prevent Windows from using all the hardware available.",
            ),
            Self::NoOnlineAccount => (
                "Remove requirement for an online Microsoft account",
                "For this option to work, network/Internet MUST be disconnected during installation!",
            ),
            Self::LocalAccount => (
                "Create a local account with username:",
                "Automatically create a local account with the specified user name, using an empty password that will need to be changed on next logon.",
            ),
            Self::Regional => (
                "Set regional options to the same values as this user's",
                "Duplicate the regional settings from this PC (keyboard, timezone, currency), instead of prompting the user.",
            ),
            Self::NoDataCollection => (
                "Disable data collection (Skip privacy questions)",
                "Automatically answer 'no' to the Windows setup questions relating to the sharing of data with Microsoft, instead of prompting the user.",
            ),
            Self::SilentInstall => (
                "⚠SILENTLY⚠ erase disk and install:",
                "If you use this option, please make sure to disconnect every disk from the target PC, except the one you want to install Windows on, as well as not leave the media plugged into any PC you don't want to erase.",
            ),
            Self::DisableBitlocker => (
                "Disable BitLocker automatic device encryption",
                "Don't encrypt the system disk, unless explicitly requested by the user.",
            ),
            Self::QualityOfLife => (
                "QoL improvements (Don't force Copilot, OneDrive, Outlook, Fast Startup, etc.)",
                "\"Quality of Life\" enhancements: Disable most of the unwanted features Microsoft is trying to force onto end users.",
            ),
            Self::ApplySkuSiPolicy => (
                "Apply SkuSiPolicy.p7b on installation (See KB5042562)",
                "Use this option if you want to revoke additional potentially unsafe Windows bootloaders, but with the potential of also preventing standard Windows media from booting.",
            ),
            Self::ForceSMode => (
                "Restrict Windows to S-Mode (INCOMPATIBLE with online account bypass)",
                "Use this option only if you know what S-Mode is and understand that your system may be locked into S-Mode even after you completely erase and reinstall Windows.",
            ),
        }
    }

    /// Name in `settings.conf`.
    pub fn key(self) -> &'static str {
        match self {
            Self::BypassRequirements => "bypass",
            Self::NoOnlineAccount => "no-online-account",
            Self::LocalAccount => "local-account",
            Self::Regional => "regional",
            Self::NoDataCollection => "no-data-collection",
            Self::SilentInstall => "silent",
            Self::DisableBitlocker => "no-bitlocker",
            Self::QualityOfLife => "qol",
            Self::ApplySkuSiPolicy => "skusipolicy",
            Self::ForceSMode => "s-mode",
        }
    }

    const PERSISTED: [Self; 8] = [
        Self::BypassRequirements,
        Self::NoOnlineAccount,
        Self::LocalAccount,
        Self::Regional,
        Self::NoDataCollection,
        Self::DisableBitlocker,
        Self::QualityOfLife,
        Self::ApplySkuSiPolicy,
    ];

    pub fn from_key(key: &str) -> Option<Self> {
        Self::PERSISTED
            .into_iter()
            .find(|option| option.key() == key)
    }
}

/// Upstream's `UNATTEND_DEFAULT_SELECTION_MASK`.
pub fn default_selection() -> Vec<WueOption> {
    vec![WueOption::BypassRequirements, WueOption::NoOnlineAccount]
}

/// Silent install and S Mode are never remembered, as upstream's
/// `UNATTEND_DEFAULT_MASK` leaves them out.
pub fn remembered(selection: &[WueOption]) -> Vec<WueOption> {
    WueOption::PERSISTED
        .into_iter()
        .filter(|option| selection.contains(option))
        .collect()
}

/// Whether Start shows the dialog for this image, as upstream's BootCheck
/// decides for non Windows To Go media.
pub fn applies(windows: &WindowsImage) -> bool {
    windows.is_windows_10_or_later() && !windows.has_panther_unattend
}

/// The options upstream lists for this image, in its order.
pub fn offered(windows: &WindowsImage, expert: bool) -> Vec<WueOption> {
    let mut options = Vec::new();
    let windows_11 = windows.is_windows_11();
    if windows_11 {
        options.push(WueOption::BypassRequirements);
    }
    if windows.build >= 22500 {
        options.push(WueOption::NoOnlineAccount);
    }
    options.extend([
        WueOption::LocalAccount,
        WueOption::Regional,
        WueOption::NoDataCollection,
    ]);
    if windows_11 {
        if !windows.editions.is_empty() {
            options.push(WueOption::SilentInstall);
        }
        options.extend([WueOption::DisableBitlocker, WueOption::QualityOfLife]);
        if windows.build >= 26200 {
            options.push(WueOption::ApplySkuSiPolicy);
        }
    }
    if expert {
        options.push(WueOption::ForceSMode);
    }
    options
}

/// The silent install answers every Setup question, so upstream allows it
/// only with the account, regional, and privacy answers selected.
pub fn silent_allowed(selection: &[WueOption]) -> bool {
    [
        WueOption::LocalAccount,
        WueOption::Regional,
        WueOption::NoDataCollection,
    ]
    .iter()
    .all(|option| selection.contains(option))
}

/// The default edition for a silent install: Pro when present, as upstream
/// matches the host's edition, else the first.
pub fn default_edition(windows: &WindowsImage) -> usize {
    windows
        .editions
        .iter()
        .position(|edition| edition.name.ends_with(" Pro"))
        .unwrap_or(0)
}

/// Upstream's default account name is the current user's.
pub fn default_username() -> String {
    std::env::var("USER")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "User".into())
}

pub struct Choices<'a> {
    pub selection: &'a [WueOption],
    pub username: &'a str,
    pub edition_index: Option<u32>,
    pub regional: Option<RegionalSettings>,
}

/// The request for the selected options; `None` when nothing is selected.
pub fn customization(choices: Choices<'_>) -> Option<WindowsCustomization> {
    let has = |option| choices.selection.contains(&option);
    let username = choices.username.trim();
    let customization = WindowsCustomization {
        bypass_requirements: has(WueOption::BypassRequirements),
        no_online_account: has(WueOption::NoOnlineAccount),
        local_account: (has(WueOption::LocalAccount) && !username.is_empty()).then(|| {
            username
                .chars()
                .take(rufus_helper_protocol::MAX_USERNAME_CHARS)
                .collect()
        }),
        regional: if has(WueOption::Regional) {
            choices.regional
        } else {
            None
        },
        no_data_collection: has(WueOption::NoDataCollection),
        disable_bitlocker: has(WueOption::DisableBitlocker),
        quality_of_life: has(WueOption::QualityOfLife),
        apply_skusipolicy: has(WueOption::ApplySkuSiPolicy),
        silent_install_index: if has(WueOption::SilentInstall) && silent_allowed(choices.selection)
        {
            choices.edition_index
        } else {
            None
        },
        force_s_mode: has(WueOption::ForceSMode),
    };
    (!customization.is_empty()).then_some(customization)
}

/// One line per selected option for the destructive confirmation.
pub fn summary(customization: &WindowsCustomization) -> String {
    let mut lines = Vec::new();
    let mut add = |on: bool, text: String| {
        if on {
            lines.push(format!("• {text}"));
        }
    };
    add(
        customization.bypass_requirements,
        "No TPM 2.0, Secure Boot, or 4 GB RAM requirement".into(),
    );
    add(
        customization.no_online_account,
        "No online Microsoft account requirement".into(),
    );
    if let Some(name) = &customization.local_account {
        add(true, format!("Local account “{name}”"));
    }
    add(
        customization.regional.is_some(),
        "This computer's regional options".into(),
    );
    add(
        customization.no_data_collection,
        "No data collection".into(),
    );
    add(
        customization.disable_bitlocker,
        "No BitLocker device encryption".into(),
    );
    add(
        customization.quality_of_life,
        "Quality of life improvements".into(),
    );
    add(
        customization.apply_skusipolicy,
        "SkuSiPolicy.p7b applied".into(),
    );
    add(customization.force_s_mode, "S Mode enforced".into());
    if let Some(index) = customization.silent_install_index {
        add(
            true,
            format!("⚠ SILENT install of image {index}: the first disk of the PC it boots is erased without asking"),
        );
    }
    format!("Windows customization:\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rufus_image::windows::WindowsEdition;

    fn windows(major: u32, build: u32) -> WindowsImage {
        WindowsImage {
            major,
            build,
            has_bootmgr_efi: true,
            editions: vec![
                WindowsEdition {
                    index: 1,
                    name: "Windows 11 Home".into(),
                    display_name: "Windows 11 Home".into(),
                },
                WindowsEdition {
                    index: 6,
                    name: "Windows 11 Pro".into(),
                    display_name: "Windows 11 Pro".into(),
                },
            ],
            ..WindowsImage::default()
        }
    }

    #[test]
    fn options_follow_upstream_version_rules() {
        use WueOption::*;
        assert_eq!(
            offered(&windows(10, 19045), false),
            [LocalAccount, Regional, NoDataCollection]
        );
        assert_eq!(
            offered(&windows(11, 22631), false),
            [
                BypassRequirements,
                NoOnlineAccount,
                LocalAccount,
                Regional,
                NoDataCollection,
                SilentInstall,
                DisableBitlocker,
                QualityOfLife
            ]
        );
        let latest = offered(&windows(11, 26300), true);
        assert!(latest.contains(&ApplySkuSiPolicy));
        assert_eq!(latest.last(), Some(&ForceSMode));
    }

    #[test]
    fn the_dialog_needs_windows_10_media_without_its_own_answer_file() {
        assert!(applies(&windows(11, 26100)));
        assert!(!applies(&windows(8, 9600)));
        let mut no_efi = windows(11, 26100);
        no_efi.has_bootmgr_efi = false;
        assert!(!applies(&no_efi));
        let mut answered = windows(11, 26100);
        answered.has_panther_unattend = true;
        assert!(!applies(&answered));
        assert_eq!(default_edition(&windows(11, 26100)), 1);
    }

    #[test]
    fn silent_install_needs_every_answer_and_is_never_remembered() {
        use WueOption::*;
        let mut selection = vec![SilentInstall, LocalAccount, NoDataCollection];
        fn choices(selection: &[WueOption]) -> Choices<'_> {
            Choices {
                selection,
                username: " ana ",
                edition_index: Some(6),
                regional: Some(RegionalSettings::default()),
            }
        }
        let partial = customization(choices(&selection)).expect("options");
        assert_eq!(partial.silent_install_index, None);
        assert_eq!(partial.local_account.as_deref(), Some("ana"));
        selection.push(Regional);
        let full = customization(choices(&selection)).expect("options");
        assert_eq!(full.silent_install_index, Some(6));
        assert!(summary(&full).contains("SILENT"));
        assert_eq!(
            remembered(&selection),
            [LocalAccount, Regional, NoDataCollection]
        );
        assert_eq!(
            customization(Choices {
                selection: &[],
                username: "",
                edition_index: None,
                regional: None
            }),
            None
        );
    }

    #[test]
    fn keys_round_trip_for_remembered_options() {
        for option in WueOption::PERSISTED {
            assert_eq!(WueOption::from_key(option.key()), Some(option));
        }
        assert_eq!(WueOption::from_key("silent"), None);
        assert_eq!(WueOption::from_key("s-mode"), None);
    }
}
