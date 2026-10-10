//! What upstream Rufus learns from a Windows installer ISO before offering
//! the Windows User Experience options: the Windows version and editions of
//! the install image, its architecture, and the setup language.

use std::fs::File;

use crate::isofs::{self, IsoListing};
use crate::wim;

const INSTALL_IMAGES: [&str; 3] = [
    "sources/install.wim",
    "sources/install.esd",
    "sources/install.swm",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowsImage {
    /// 7, 8, 10, or 11, as upstream reports it (10.0 with a build above
    /// 20000 is Windows 11); 0 when the version could not be read.
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub arch: Option<WindowsArch>,
    pub editions: Vec<WindowsEdition>,
    /// Languages of the install image, such as `en-US`.
    pub languages: Vec<String>,
    /// The language of Windows Setup, from `sources/boot.wim`.
    pub setup_language: Option<String>,
    /// `bootmgr.efi` is present; upstream requires it for every option.
    pub has_bootmgr_efi: bool,
    /// The ISO ships its own `sources/$OEM$/$$/Panther/unattend.xml`, which
    /// upstream leaves alone instead of offering its options.
    pub has_panther_unattend: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsEdition {
    pub index: u32,
    pub name: String,
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsArch {
    X86,
    Amd64,
    Arm,
    Arm64,
}

impl WindowsArch {
    /// The `processorArchitecture` of answer-file components.
    pub const fn unattend_name(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::Amd64 => "amd64",
            Self::Arm => "arm",
            Self::Arm64 => "arm64",
        }
    }

    /// `PROCESSOR_ARCHITECTURE_*` values used by the WIM index.
    fn from_wim(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::X86),
            5 => Some(Self::Arm),
            9 => Some(Self::Amd64),
            12 => Some(Self::Arm64),
            _ => None,
        }
    }
}

impl WindowsImage {
    /// Upstream's `IS_WINDOWS_1X`.
    pub fn is_windows_10_or_later(&self) -> bool {
        self.has_bootmgr_efi && self.major >= 10
    }

    /// Upstream's `IS_WINDOWS_11`.
    pub fn is_windows_11(&self) -> bool {
        self.has_bootmgr_efi && self.major >= 11
    }
}

/// Read the Windows details of an ISO whose listing has an install image.
pub fn inspect(file: &mut File, listing: &IsoListing) -> Option<WindowsImage> {
    let install = INSTALL_IMAGES.iter().find_map(|path| listing.find(path))?;
    let images = read_index(file, install).unwrap_or_default();
    let mut windows = WindowsImage {
        has_bootmgr_efi: listing.has_file("bootmgr.efi"),
        has_panther_unattend: listing.has_file("sources/$OEM$/$$/Panther/unattend.xml"),
        ..WindowsImage::default()
    };
    if let Some(first) = images.first() {
        (windows.major, windows.minor) = marketing_version(first.major, first.minor, first.build);
        windows.build = first.build;
        windows.arch = first.arch.and_then(WindowsArch::from_wim);
        windows.languages = first.languages.clone();
    }
    windows.arch = windows.arch.or_else(|| arch_from_loaders(listing));
    windows.editions = images
        .into_iter()
        .map(|image| WindowsEdition {
            index: image.index,
            name: image.name,
            display_name: image.display_name,
        })
        .collect();
    windows.setup_language = listing
        .find("sources/boot.wim")
        .and_then(|boot| read_index(file, boot).ok())
        .and_then(|images| {
            // Windows Setup is image 2 of official media.
            let setup = images
                .iter()
                .find(|image| image.index == 2)
                .or(images.first())?;
            setup.languages.first().cloned()
        });
    Some(windows)
}

fn read_index(file: &mut File, entry: &isofs::IsoEntry) -> std::io::Result<Vec<wim::WimImage>> {
    let header = isofs::read_range(file, entry, 0, wim::HEADER_BYTES)?;
    let (offset, size) = wim::xml_location(&header)?;
    let bytes = isofs::read_range(file, entry, offset, size)?;
    if bytes.len() != size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the WIM index is truncated",
        ));
    }
    Ok(wim::images(&wim::decode_xml(&bytes)?))
}

/// Upstream's `PopulateWindowsVersionFromXml` adjustments.
fn marketing_version(major: u32, minor: u32, build: u32) -> (u32, u32) {
    match (major, minor) {
        (0..=5, _) | (6, 0) => (0, 0),
        (6, 1) => (7, 0),
        (6, 2) => (8, 0),
        (6, 3) => (8, 1),
        (6, 4) => (10, 0),
        (10, _) if build > 20000 => (11, minor),
        other => other,
    }
}

fn arch_from_loaders(listing: &IsoListing) -> Option<WindowsArch> {
    [
        ("efi/boot/bootaa64.efi", WindowsArch::Arm64),
        ("efi/boot/bootarm.efi", WindowsArch::Arm),
        ("efi/boot/bootx64.efi", WindowsArch::Amd64),
        ("efi/boot/bootia32.efi", WindowsArch::X86),
    ]
    .into_iter()
    .find(|(path, _)| listing.has_file(path))
    .map(|(_, arch)| arch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_follow_upstream_naming() {
        assert_eq!(marketing_version(10, 0, 19045), (10, 0));
        assert_eq!(marketing_version(10, 0, 22631), (11, 0));
        assert_eq!(marketing_version(6, 1, 7601), (7, 0));
        assert_eq!(marketing_version(6, 3, 9600), (8, 1));
        assert_eq!(marketing_version(6, 0, 6002), (0, 0));
        assert_eq!(marketing_version(5, 1, 2600), (0, 0));
    }

    #[test]
    fn reads_version_editions_and_setup_language_from_the_image() {
        use crate::isofs::fixture::{udf, Node};
        let install = crate::wim::fixture(
            r#"<WIM><IMAGE INDEX="1"><NAME>Windows 11 Home</NAME><DISPLAYNAME>Windows 11 Home</DISPLAYNAME>
<WINDOWS><ARCH>9</ARCH><VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD></VERSION></WINDOWS></IMAGE>
<IMAGE INDEX="2"><NAME>Windows 11 Pro</NAME><DISPLAYNAME>Windows 11 Pro</DISPLAYNAME>
<WINDOWS><ARCH>9</ARCH><VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD></VERSION></WINDOWS></IMAGE></WIM>"#,
        );
        let boot = crate::wim::fixture(
            r#"<WIM><IMAGE INDEX="1"><WINDOWS><LANGUAGES><LANGUAGE>en-US</LANGUAGE></LANGUAGES></WINDOWS></IMAGE>
<IMAGE INDEX="2"><WINDOWS><LANGUAGES><LANGUAGE>fr-FR</LANGUAGE></LANGUAGES></WINDOWS></IMAGE></WIM>"#,
        );
        let image = udf(vec![
            Node::File("bootmgr.efi", 1000),
            Node::Dir(
                "sources",
                vec![
                    Node::Bytes("install.wim", install),
                    Node::Bytes("boot.wim", boot),
                ],
            ),
        ]);
        let path = std::env::temp_dir().join(format!("rufus-windows-{}.iso", std::process::id()));
        std::fs::write(&path, image).expect("write fixture");
        let mut file = File::open(&path).expect("open fixture");
        let listing = isofs::list(&mut file).expect("list").expect("listing");
        let windows = inspect(&mut file, &listing).expect("Windows image");
        std::fs::remove_file(&path).expect("remove fixture");
        assert_eq!((windows.major, windows.build), (11, 26100));
        assert_eq!(windows.arch, Some(WindowsArch::Amd64));
        assert!(windows.is_windows_11());
        assert!(!windows.has_panther_unattend);
        assert_eq!(windows.setup_language.as_deref(), Some("fr-FR"));
        let names: Vec<_> = windows
            .editions
            .iter()
            .map(|e| (e.index, e.display_name.as_str()))
            .collect();
        assert_eq!(names, [(1, "Windows 11 Home"), (2, "Windows 11 Pro")]);
    }

    /// Run with `RUFUS_WINDOWS_ISO=/path/to.iso cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a real Windows ISO"]
    fn reads_a_real_windows_iso() {
        let path = std::env::var("RUFUS_WINDOWS_ISO").expect("RUFUS_WINDOWS_ISO");
        let mut file = File::open(path).expect("open ISO");
        let listing = isofs::list(&mut file).expect("read ISO").expect("listing");
        let windows = inspect(&mut file, &listing).expect("Windows ISO");
        eprintln!("{windows:#?}");
        assert!(windows.major >= 10 && windows.build > 0);
        assert!(!windows.editions.is_empty());
        assert!(windows.setup_language.is_some());
    }
}
