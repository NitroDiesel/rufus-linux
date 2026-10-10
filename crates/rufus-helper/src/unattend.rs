//! Windows answer files for the Windows User Experience options, ported from
//! upstream Rufus's `CreateUnattendXml` (`src/wue.c`).
//!
//! Upstream adds an answer file with a `windowsPE` pass to `boot.wim` and
//! tries to write the hardware bypass straight into its offline registry,
//! falling back to `RunSynchronous` commands when it cannot. Rufus Linux does
//! not rewrite WIM files, so it always uses that fallback and places the file
//! as `autounattend.xml` at the root of the media, where Windows Setup looks
//! for it on removable drives and carries it into the later passes. Without
//! a `windowsPE` pass the file goes to `sources/$OEM$/$$/Panther/unattend.xml`
//! as upstream does.

use std::fmt::Write as _;

use rufus_helper_protocol::WindowsCustomization;
use rufus_image::windows::WindowsArch;

const BYPASS_NAMES: [&str; 3] = ["BypassTPMCheck", "BypassSecureBootCheck", "BypassRAMCheck"];

/// Names Windows refuses for a new local account, from upstream.
const RESERVED_ACCOUNT_NAMES: [&str; 15] = [
    "Administrator",
    "Järjestelmänvalvoja",
    "Administrateur",
    "Rendszergazda",
    "Administrador",
    "Администратор",
    "Administratör",
    "Guest",
    "DefaultAccount",
    "WDAGUtilityAccount",
    "HelpAssistant",
    "KRBTGT",
    "Local",
    "NONE",
    "SYSTEM",
];

/// Upstream's `USERNAME_INVALID_CHARS`, replaced by `_`.
const INVALID_ACCOUNT_CHARS: &str = "/\\[]:;|=.,+*?<>%@&\"";

pub(crate) const ROOT_ANSWER_FILE: &str = "autounattend.xml";
pub(crate) const PANTHER_ANSWER_FILE: &str = "sources/$OEM$/$$/Panther/unattend.xml";

pub(crate) struct AnswerFile {
    pub xml: String,
    /// Where on the media the file belongs.
    pub path: &'static str,
    /// What was applied, for the log, as upstream prints it.
    pub log: Vec<String>,
}

/// Whether Windows Setup itself (the `windowsPE` pass) needs the file.
pub(crate) fn has_setup_pass(options: &WindowsCustomization) -> bool {
    options.bypass_requirements || options.silent_install_index.is_some()
}

/// Upstream's account name clean-up: invalid characters become `_`, the
/// ends are trimmed, and reserved names drop the option.
pub(crate) fn account_name(requested: &str) -> Result<String, String> {
    let cleaned: String = requested
        .chars()
        .map(|c| {
            if INVALID_ACCOUNT_CHARS.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().to_owned();
    if cleaned.is_empty() {
        return Err("an empty local account name".into());
    }
    if RESERVED_ACCOUNT_NAMES
        .iter()
        .any(|reserved| reserved.to_lowercase() == cleaned.to_lowercase())
    {
        return Err(format!(
            "'{cleaned}' is not allowed as a local account name"
        ));
    }
    Ok(cleaned)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn component(out: &mut String, name: &str, arch: WindowsArch) {
    let _ = writeln!(
        out,
        "    <component name=\"{name}\" processorArchitecture=\"{}\" language=\"neutral\" \
         xmlns:wcm=\"http://schemas.microsoft.com/WMIConfig/2002/State\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         publicKeyToken=\"31bf3856ad364e35\" versionScope=\"nonSxS\">",
        arch.unattend_name()
    );
}

pub(crate) fn answer_file(
    options: &WindowsCustomization,
    arch: WindowsArch,
    setup_language: &str,
) -> AnswerFile {
    let mut out = String::new();
    let mut log = vec!["Selected Windows User Experience options:".to_owned()];
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<unattend xmlns=\"urn:schemas-microsoft-com:unattend\">\n");

    let silent = options.silent_install_index;
    if has_setup_pass(options) {
        out.push_str("  <settings pass=\"windowsPE\">\n");
        component(&mut out, "Microsoft-Windows-Setup", arch);
        // Windows Setup insists on a product key, even an empty one.
        out.push_str(
            "      <UserData>\n        <AcceptEula>true</AcceptEula>\n        <ProductKey>\n          \
             <Key />\n        </ProductKey>\n      </UserData>\n",
        );
        if let Some(index) = silent {
            log.push("• ⚠Silent Install⚠".into());
            silent_install(&mut out, index, options.disable_bitlocker);
        }
        if options.bypass_requirements {
            log.push("• Bypass SB/TPM/RAM".into());
            out.push_str("      <RunSynchronous>\n");
            for (order, name) in BYPASS_NAMES.iter().enumerate() {
                let _ = write!(
                    out,
                    "        <RunSynchronousCommand wcm:action=\"add\">\n          \
                     <Order>{}</Order>\n          <Path>reg add HKLM\\SYSTEM\\Setup\\LabConfig \
                     /v {name} /t REG_DWORD /d 1 /f</Path>\n        </RunSynchronousCommand>\n",
                    order + 1
                );
            }
            out.push_str("      </RunSynchronous>\n");
        }
        out.push_str("    </component>\n");
        if silent.is_some() {
            component(&mut out, "Microsoft-Windows-International-Core-WinPE", arch);
            let _ = writeln!(
                out,
                "      <UILanguage>{}</UILanguage>",
                escape(setup_language)
            );
            out.push_str("    </component>\n");
        }
        out.push_str("  </settings>\n");
    }

    if options.no_online_account || options.quality_of_life {
        let mut commands = Vec::new();
        if options.no_online_account {
            log.push("• Bypass online account requirement".into());
            commands.push(
                r#"reg add "HKLM\Software\Microsoft\Windows\CurrentVersion\OOBE" /v BypassNRO /t REG_DWORD /d 1 /f"#
                    .to_owned(),
            );
        }
        if options.quality_of_life {
            log.push("• QoL: Disable OneDrive and Outlook by default".into());
            commands.extend(SPECIALIZE_QOL_COMMANDS.iter().map(|c| (*c).to_owned()));
        }
        out.push_str("  <settings pass=\"specialize\">\n");
        component(&mut out, "Microsoft-Windows-Deployment", arch);
        command_list(
            &mut out,
            "RunSynchronous",
            "RunSynchronousCommand",
            "Path",
            &commands,
        );
        out.push_str("    </component>\n  </settings>\n");
    }

    let account = options.local_account.as_deref().map(account_name);
    let shell_setup = options.no_data_collection
        || silent.is_some()
        || options.regional.is_some()
        || options.local_account.is_some()
        || options.apply_skusipolicy
        || options.quality_of_life;
    if shell_setup || options.disable_bitlocker {
        out.push_str("  <settings pass=\"oobeSystem\">\n");
        if shell_setup {
            component(&mut out, "Microsoft-Windows-Shell-Setup", arch);
            let mut commands = Vec::new();
            if options.no_data_collection || silent.is_some() {
                log.push("• Disable data collection".into());
                out.push_str("      <OOBE>\n        <HideEULAPage>true</HideEULAPage>\n");
                out.push_str("        <ProtectYourPC>3</ProtectYourPC>\n");
                // Otherwise "Let's connect you to a network" can still appear.
                if silent.is_some() {
                    out.push_str(
                        "        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>\n        \
                         <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>\n",
                    );
                }
                out.push_str("      </OOBE>\n");
            }
            if let Some(zone) = options
                .regional
                .as_ref()
                .and_then(|r| r.time_zone.as_deref())
            {
                let _ = writeln!(out, "      <TimeZone>{}</TimeZone>", escape(zone));
            }
            match &account {
                Some(Ok(name)) => {
                    log.push(format!("• Use '{name}' for local account name"));
                    let name_xml = escape(name);
                    let _ = write!(
                        out,
                        "      <UserAccounts>\n        <LocalAccounts>\n          \
                         <LocalAccount wcm:action=\"add\">\n            <Name>{name_xml}</Name>\n            \
                         <DisplayName>{name_xml}</DisplayName>\n            \
                         <Group>Administrators;Power Users</Group>\n            <Password>\n              \
                         <Value>UABhAHMAcwB3AG8AcgBkAA==</Value>\n              \
                         <PlainText>false</PlainText>\n            </Password>\n          \
                         </LocalAccount>\n        </LocalAccounts>\n      </UserAccounts>\n"
                    );
                    // The password is empty, so ask for one at first logon,
                    // and keep `net user` from expiring it after 90 days.
                    commands.push(format!("net user \"{name}\" /logonpasswordchg:yes"));
                    commands.push("net accounts /maxpwage:unlimited".into());
                }
                Some(Err(reason)) => log.push(format!("WARNING: {reason} - Option ignored")),
                None => {}
            }
            if options.apply_skusipolicy {
                log.push("• Apply SkuSiPolicy.p7b".into());
                commands.push(
                    "cmd /c mountvol S: /S && copy %WINDIR%\\system32\\SecureBootUpdates\\SkuSiPolicy.p7b \
                     S:\\EFI\\Microsoft\\Boot && mountvol S: /D"
                        .into(),
                );
            }
            if options.quality_of_life {
                log.push(
                    "• QoL: Disable Fast Startup, Copilot, Recommendations, News and Teams by default"
                        .into(),
                );
                log.push("• QoL: More pins for the Start Menu and enable useful shortcuts".into());
                log.push("• QoL: Restore classic context menu".into());
                commands.extend(FIRST_LOGON_QOL_COMMANDS.iter().map(|c| (*c).to_owned()));
            }
            command_list(
                &mut out,
                "FirstLogonCommands",
                "SynchronousCommand",
                "CommandLine",
                &commands,
            );
            out.push_str("    </component>\n");
        }
        if let Some(regional) = &options.regional {
            log.push("• Use the same regional options as this user's".into());
            component(&mut out, "Microsoft-Windows-International-Core", arch);
            let _ = write!(
                out,
                "      <InputLocale>{}</InputLocale>\n      <SystemLocale>{}</SystemLocale>\n      \
                 <UserLocale>{}</UserLocale>\n      <UILanguage>{}</UILanguage>\n      \
                 <UILanguageFallback>en-US</UILanguageFallback>\n",
                escape(&regional.input_locale),
                escape(&regional.system_locale),
                escape(&regional.user_locale),
                escape(&regional.ui_language),
            );
            out.push_str("    </component>\n");
        }
        if options.disable_bitlocker {
            log.push("• Disable BitLocker".into());
            component(
                &mut out,
                "Microsoft-Windows-SecureStartup-FilterDriver",
                arch,
            );
            out.push_str(
                "      <PreventDeviceEncryption>true</PreventDeviceEncryption>\n    </component>\n",
            );
            component(&mut out, "Microsoft-Windows-EnhancedStorage-Adm", arch);
            out.push_str(
                "      <TCGSecurityActivationDisabled>1</TCGSecurityActivationDisabled>\n    </component>\n",
            );
        }
        out.push_str("  </settings>\n");
    }

    if options.force_s_mode {
        log.push("• Enforce S Mode".into());
        out.push_str("  <settings pass=\"offlineServicing\">\n");
        component(&mut out, "Microsoft-Windows-CodeIntegrity", arch);
        out.push_str(
            "      <SkuPolicyRequired>1</SkuPolicyRequired>\n    </component>\n  </settings>\n",
        );
    }

    out.push_str("</unattend>\n");
    AnswerFile {
        xml: out,
        path: if has_setup_pass(options) {
            ROOT_ANSWER_FILE
        } else {
            PANTHER_ANSWER_FILE
        },
        log,
    }
}

fn command_list(out: &mut String, list: &str, item: &str, field: &str, commands: &[String]) {
    if commands.is_empty() {
        return;
    }
    let _ = writeln!(out, "      <{list}>");
    for (order, command) in commands.iter().enumerate() {
        let _ = write!(
            out,
            "        <{item} wcm:action=\"add\">\n          <Order>{}</Order>\n          \
             <{field}>{}</{field}>\n        </{item}>\n",
            order + 1,
            escape(command)
        );
    }
    let _ = writeln!(out, "      </{list}>");
}

/// Let Windows Setup erase disk 0 and install without asking. The boot
/// media's own partition 2 is labelled first: if that fails because the
/// media is not the only other disk, Setup shows its disk screen instead of
/// guessing, which keeps the media from being erased.
fn silent_install(out: &mut String, index: u32, disable_bitlocker: bool) {
    out.push_str("      <DiskConfiguration>\n        <WillShowUI>OnError</WillShowUI>\n");
    if disable_bitlocker {
        out.push_str(
            "        <DisableEncryptedDiskProvisioning>true</DisableEncryptedDiskProvisioning>\n",
        );
    }
    let _ = write!(
        out,
        r#"        <Disk wcm:action="modify">
          <DiskID>1</DiskID>
          <ModifyPartitions>
            <ModifyPartition wcm:action="modify">
              <Order>1</Order>
              <PartitionID>2</PartitionID>
              <Label>RUFUS_BOOT</Label>
            </ModifyPartition>
          </ModifyPartitions>
        </Disk>
        <Disk wcm:action="add">
          <DiskID>0</DiskID>
          <WillWipeDisk>true</WillWipeDisk>
          <CreatePartitions>
            <CreatePartition wcm:action="add">
              <Order>1</Order>
              <Type>EFI</Type>
              <Size>260</Size>
            </CreatePartition>
            <CreatePartition wcm:action="add">
              <Order>2</Order>
              <Type>MSR</Type>
              <Size>16</Size>
            </CreatePartition>
            <CreatePartition wcm:action="add">
              <Order>3</Order>
              <Type>Primary</Type>
              <Extend>true</Extend>
            </CreatePartition>
          </CreatePartitions>
          <ModifyPartitions>
            <ModifyPartition wcm:action="add">
              <Order>1</Order>
              <PartitionID>1</PartitionID>
              <Label>EFI</Label>
              <Format>FAT32</Format>
            </ModifyPartition>
            <ModifyPartition wcm:action="add">
              <Order>2</Order>
              <PartitionID>3</PartitionID>
              <Label>Windows</Label>
              <Letter>C</Letter>
              <Format>NTFS</Format>
            </ModifyPartition>
          </ModifyPartitions>
        </Disk>
      </DiskConfiguration>
      <ImageInstall>
        <OSImage>
          <WillShowUI>OnError</WillShowUI>
          <InstallFrom>
            <MetaData wcm:action="add">
              <Key>/IMAGE/INDEX</Key>
              <Value>{index}</Value>
            </MetaData>
          </InstallFrom>
          <InstallTo>
            <DiskID>0</DiskID>
            <PartitionID>3</PartitionID>
          </InstallTo>
        </OSImage>
      </ImageInstall>
"#
    );
}

const SPECIALIZE_QOL_COMMANDS: &[&str] = &[
    r#"reg add "HKLM\Software\Policies\Microsoft\Windows\OneDrive" /v DisableFileSyncNGSC /t REG_DWORD /d 1 /f"#,
    r#"PowerShell -NonInteractive -WindowStyle Hidden -Command "Remove-Item -Path $env:SystemRoot\System32\OneDriveSetup.exe -Force -Confirm:$false; Remove-Item -Path $env:SystemRoot\SysWOW64\OneDriveSetup.exe -Force -Confirm:$false;""#,
    r#"PowerShell -NonInteractive -WindowStyle Hidden -Command "Get-AppxProvisionedPackage -Online | Where-Object {$_.PackageName -like '*Outlook*'} | Remove-AppxProvisionedPackage -Online""#,
    r#"PowerShell -NonInteractive -WindowStyle Hidden -Command "Get-AppxPackage -AllUsers *Outlook* | Remove-AppxPackage -AllUsers""#,
    r#"PowerShell -NonInteractive -WindowStyle Hidden -Command "Get-AppxProvisionedPackage -Online | Where-Object {$_.PackageName -like '*Teams*'} | Remove-AppxProvisionedPackage -Online""#,
    r#"PowerShell -NonInteractive -WindowStyle Hidden -Command "Get-AppxPackage -AllUsers *Teams* | Remove-AppxPackage -AllUsers""#,
];

const FIRST_LOGON_QOL_COMMANDS: &[&str] = &[
    r#"reg add "HKLM\System\CurrentControlSet\Control\Session Manager\Power" /v HiberbootEnabled /t REG_DWORD /d 0 /f"#,
    r#"reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced" /v ShowCopilotButton /t REG_DWORD /d 0 /f"#,
    r#"reg add "HKLM\Software\Policies\Microsoft\Windows\WindowsCopilot" /v TurnOffWindowsCopilot /t REG_DWORD /d 1 /f"#,
    r#"reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Search" /v SearchboxTaskbarMode /t REG_DWORD /d 1 /f"#,
    r#"reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Search" /v SearchboxTaskbarModeCache /t REG_DWORD /d 1 /f"#,
    r#"reg add "HKLM\Software\Policies\Microsoft\Windows\CloudContent" /v DisableWindowsConsumerFeatures /t REG_DWORD /d 1 /f"#,
    r#"reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\ContentDeliveryManager" /v SystemPaneSuggestionsEnabled /t REG_DWORD /d 0 /f"#,
    r#"reg add "HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Search" /v BingSearchEnabled /t REG_DWORD /d 0 /f"#,
    r#"reg add "HKLM\Software\Policies\Microsoft\Windows\Device Metadata" /v PreventDeviceMetadataFromNetwork /t REG_DWORD /d 1 /f"#,
    r#"reg add "HKLM\Software\Policies\Microsoft\Dsh" /v AllowNewsAndInterests /t REG_DWORD /d 0 /f"#,
    r#"reg add "HKLM\Software\Policies\Microsoft\Windows\Windows Feeds" /v EnableFeeds /t REG_DWORD /d 0 /f"#,
    r#"reg add "HKLM\Software\Microsoft\Windows\CurrentVersion\Communications" /v ConfigureChatAutoInstall /t REG_DWORD /d 0 /f"#,
    r#"reg add "HKLM\Software\Policies\Microsoft\Windows\CloudContent" /v DisableCloudOptimizedContent /t REG_DWORD /d 1 /f"#,
    r#"reg add "HKLM\Software\Policies\Microsoft\Edge" /v HideFirstRunExperience /t REG_DWORD /d 1 /f"#,
    r#"reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced" /v Start_Layout /t REG_DWORD /d 1 /f"#,
    r#"PowerShell -NonInteractive -WindowStyle Hidden -Command "Set-ItemProperty -Path 'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Start' -Name 'VisiblePlaces' -Value $([convert]::FromBase64String('ztU0LVr6Q0WC8iLm6vd3PC+zZ+PeiVVDv85h83sYqTe8JIoUDNaJQqCAbtm7okiCRIF1/g0IrkKL2jTtl7ZjlEqwvXRK+WhPi9ZDmAcdqLyGCHNSqlFDQp97J3ZYRlnU')) -Type 'Binary'""#,
    r#"reg add "HKCU\Software\Classes\CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\InprocServer32" /ve /t REG_SZ /d "" /f"#,
];

#[cfg(test)]
mod tests {
    use super::*;
    use rufus_helper_protocol::RegionalSettings;

    fn defaults() -> WindowsCustomization {
        WindowsCustomization {
            bypass_requirements: true,
            no_online_account: true,
            ..WindowsCustomization::default()
        }
    }

    fn well_formed(xml: &str) {
        // Every element opened is closed, in order.
        let mut stack = Vec::new();
        let mut rest = xml;
        while let Some(start) = rest.find('<') {
            let end = rest[start..].find('>').expect("tag end") + start;
            let tag = &rest[start + 1..end];
            rest = &rest[end + 1..];
            if tag.starts_with('?') || tag.ends_with('/') {
                continue;
            }
            let name = tag
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .expect("tag name");
            if tag.starts_with('/') {
                assert_eq!(stack.pop(), Some(name), "unbalanced </{name}>");
            } else {
                stack.push(name);
            }
        }
        assert!(stack.is_empty(), "unclosed {stack:?}");
    }

    #[test]
    fn upstream_defaults_bypass_checks_in_setup_and_the_account_in_specialize() {
        let file = answer_file(&defaults(), WindowsArch::Amd64, "en-US");
        well_formed(&file.xml);
        assert_eq!(file.path, ROOT_ANSWER_FILE);
        assert!(file.xml.contains("<settings pass=\"windowsPE\">"));
        assert!(file.xml.contains("processorArchitecture=\"amd64\""));
        for name in BYPASS_NAMES {
            assert!(file
                .xml
                .contains(&format!("/v {name} /t REG_DWORD /d 1 /f")));
        }
        assert!(file.xml.contains("<settings pass=\"specialize\">"));
        assert!(file.xml.contains("/v BypassNRO /t REG_DWORD /d 1 /f"));
        assert!(!file.xml.contains("oobeSystem"));
        assert!(!file.xml.contains("UILanguage"));
    }

    #[test]
    fn without_setup_options_the_file_goes_to_panther() {
        let options = WindowsCustomization {
            no_data_collection: true,
            ..WindowsCustomization::default()
        };
        let file = answer_file(&options, WindowsArch::Arm64, "en-US");
        well_formed(&file.xml);
        assert_eq!(file.path, PANTHER_ANSWER_FILE);
        assert!(!file.xml.contains("windowsPE"));
        assert!(file.xml.contains("<ProtectYourPC>3</ProtectYourPC>"));
        assert!(file.xml.contains("processorArchitecture=\"arm64\""));
    }

    #[test]
    fn every_option_together_is_well_formed_and_escaped() {
        let options = WindowsCustomization {
            bypass_requirements: true,
            no_online_account: true,
            local_account: Some("  Ana & <Bob>  ".into()),
            regional: Some(RegionalSettings {
                input_locale: "0409:00000409".into(),
                system_locale: "en-PH".into(),
                user_locale: "fil-PH".into(),
                ui_language: "en-US".into(),
                time_zone: Some("Singapore Standard Time".into()),
            }),
            no_data_collection: true,
            disable_bitlocker: true,
            quality_of_life: true,
            apply_skusipolicy: true,
            silent_install_index: Some(6),
            force_s_mode: true,
        };
        let file = answer_file(&options, WindowsArch::Amd64, "de-DE");
        well_formed(&file.xml);
        assert!(file.xml.contains("<Name>Ana _ _Bob_</Name>"));
        assert!(file
            .xml
            .contains("net user &quot;Ana _ _Bob_&quot; /logonpasswordchg:yes"));
        assert!(file.xml.contains("mountvol S: /S &amp;&amp; copy"));
        assert!(file.xml.contains("<Value>6</Value>"));
        assert!(file.xml.contains("<UILanguage>de-DE</UILanguage>"));
        assert!(file
            .xml
            .contains("<TimeZone>Singapore Standard Time</TimeZone>"));
        assert!(file
            .xml
            .contains("<HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>"));
        assert!(file.xml.contains("<DisableEncryptedDiskProvisioning>"));
        assert!(file
            .xml
            .contains("<SkuPolicyRequired>1</SkuPolicyRequired>"));
        assert!(file
            .xml
            .contains("<PreventDeviceEncryption>true</PreventDeviceEncryption>"));
        // One FirstLogonCommands list: Windows rejects a second one.
        assert_eq!(file.xml.matches("<FirstLogonCommands>").count(), 1);
        let orders: Vec<_> = file.xml.matches("<Order>").collect();
        assert!(!orders.is_empty());
    }

    #[test]
    fn reserved_or_empty_account_names_drop_the_option() {
        assert_eq!(account_name(" a.b@c ").as_deref(), Ok("a_b_c"));
        assert!(account_name("administrator").is_err());
        assert!(account_name("Администратор").is_err());
        assert!(account_name("   ").is_err());
        let options = WindowsCustomization {
            local_account: Some("Guest".into()),
            ..WindowsCustomization::default()
        };
        let file = answer_file(&options, WindowsArch::Amd64, "en-US");
        well_formed(&file.xml);
        assert!(!file.xml.contains("LocalAccount"));
        assert!(file.log.iter().any(|line| line.contains("not allowed")));
    }
}
