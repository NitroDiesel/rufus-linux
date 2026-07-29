use std::collections::BTreeMap;

/// User-visible Rufus functionality. Unavailable entries remain visible with a reason.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Capability {
    RawImageWrite,
    CompressedImageWrite,
    IsoExtraction,
    ImageCaptureDd,
    ImageCaptureVhd,
    ImageCaptureVhdx,
    ImageCaptureFfu,
    FormatFat,
    FormatFat32,
    FormatNtfs,
    FormatExfat,
    FormatUdf,
    FormatExt2,
    FormatExt3,
    FormatExt4,
    FormatRefs,
    FreeDos,
    Syslinux,
    Grub,
    UefiNtfs,
    LinuxPersistence,
    WindowsInstallerCustomization,
    WindowsToGo,
    WindowsIsoDownload,
    UefiShellDownload,
    ImageChecksums,
    UefiMediaValidation,
    BadBlocksCheck,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MissingRequirement {
    /// Stable machine-readable identifier such as `mkfs.ntfs` or `linux-only`.
    pub id: String,
    pub explanation: String,
    pub remedy: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CapabilityState {
    Available,
    Unavailable { missing: Vec<MissingRequirement> },
}

impl CapabilityState {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CapabilityReport {
    states: BTreeMap<Capability, CapabilityState>,
}

impl CapabilityReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, capability: Capability, state: CapabilityState) {
        self.states.insert(capability, state);
    }

    pub fn state(&self, capability: Capability) -> Option<&CapabilityState> {
        self.states.get(&capability)
    }

    pub fn supports(&self, capability: Capability) -> bool {
        self.state(capability)
            .is_some_and(CapabilityState::is_available)
    }

    pub fn missing<'a>(
        &'a self,
        required: impl IntoIterator<Item = Capability> + 'a,
    ) -> Vec<Capability> {
        required
            .into_iter()
            .filter(|capability| !self.supports(*capability))
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Capability, &CapabilityState)> {
        self.states
            .iter()
            .map(|(capability, state)| (*capability, state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_capabilities_are_not_assumed_available() {
        let mut report = CapabilityReport::new();
        report.set(Capability::RawImageWrite, CapabilityState::Available);
        assert!(report.supports(Capability::RawImageWrite));
        assert!(!report.supports(Capability::FormatRefs));
    }
}
