//! "Set regional options to the same values as this user's": the Linux
//! locale, keyboard layout, and time zone in Windows' answer-file notation.
//! Upstream reads the same values from the Windows registry and NLS APIs.

use std::path::Path;

use rufus_helper_protocol::RegionalSettings;

const WINDOWS_ZONES: &str = include_str!("../assets/windows-zones.txt");

/// XKB layout (and variant) to Windows `language:KLID` input locale.
const KEYBOARDS: &[(&str, &str)] = &[
    ("us", "0409:00000409"),
    ("us(intl)", "0409:00020409"),
    ("us(alt-intl)", "0409:00020409"),
    ("us(dvorak)", "0409:00010409"),
    ("gb", "0809:00000809"),
    ("ie", "1809:00001809"),
    ("ca", "0c0c:00001009"),
    ("ca(eng)", "1009:00000409"),
    ("ph", "3409:00000409"),
    ("in", "4009:00004009"),
    ("au", "0c09:00000409"),
    ("de", "0407:00000407"),
    ("at", "0c07:00000407"),
    ("ch", "0807:00000807"),
    ("ch(fr)", "100c:0000100c"),
    ("fr", "040c:0000040c"),
    ("be", "080c:0000080c"),
    ("nl", "0413:00020409"),
    ("es", "0c0a:0000040a"),
    ("latam", "080a:0000080a"),
    ("pt", "0816:00000816"),
    ("br", "0416:00000416"),
    ("it", "0410:00000410"),
    ("se", "041d:0000041d"),
    ("no", "0414:00000414"),
    ("dk", "0406:00000406"),
    ("fi", "040b:0000040b"),
    ("is", "040f:0000040f"),
    ("ee", "0425:00000425"),
    ("lv", "0426:00020426"),
    ("lt", "0427:00010427"),
    ("pl", "0415:00000415"),
    ("cz", "0405:00000405"),
    ("sk", "041b:0000041b"),
    ("hu", "040e:0000040e"),
    ("si", "0424:00000424"),
    ("hr", "041a:0000041a"),
    ("ro", "0418:00010418"),
    ("bg", "0402:00030402"),
    ("gr", "0408:00000408"),
    ("tr", "041f:0000041f"),
    ("ru", "0419:00000419"),
    ("ua", "0422:00000422"),
    ("by", "0423:00000423"),
    ("il", "040d:0000040d"),
    ("ara", "0401:00000401"),
    ("ir", "0429:00000429"),
    ("th", "041e:0000041e"),
    ("vn", "042a:0000042a"),
    ("jp", "0411:00000411"),
    ("kr", "0412:00000412"),
    ("cn", "0804:00000804"),
    ("tw", "0404:00000404"),
];

/// This session's regional settings, matched against the image's languages
/// so that Windows is never asked for a display language it does not have.
pub fn current(image_languages: &[String]) -> RegionalSettings {
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let locale = |category: &str| {
        env("LC_ALL")
            .or_else(|| env(category))
            .or_else(|| env("LANG"))
            .and_then(|value| windows_locale(&value))
            .unwrap_or_else(|| "en-US".into())
    };
    let system_locale = locale("LC_CTYPE");
    let input_locale = keyboard_layout()
        .and_then(|(layout, variant)| input_locale(&layout, variant.as_deref()))
        .unwrap_or_else(|| system_locale.clone());
    RegionalSettings {
        input_locale,
        ui_language: ui_language(&locale("LC_MESSAGES"), image_languages),
        user_locale: locale("LC_TIME"),
        system_locale,
        time_zone: time_zone_name().and_then(|zone| windows_zone(&zone)),
    }
}

/// `en_PH.UTF-8@euro` becomes `en-PH`; `C` and `POSIX` give nothing.
pub fn windows_locale(posix: &str) -> Option<String> {
    let base = posix.split(['.', '@']).next()?.trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return None;
    }
    let tag = base.replace('_', "-");
    tag.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
        .then_some(tag)
}

/// The user's language when the image has it, else the image language that
/// shares its base language, else the image's first language.
fn ui_language(wanted: &str, image_languages: &[String]) -> String {
    let base = |tag: &str| tag.split('-').next().unwrap_or(tag).to_ascii_lowercase();
    image_languages
        .iter()
        .find(|language| language.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            image_languages
                .iter()
                .find(|language| base(language) == base(wanted))
        })
        .or(image_languages.first())
        .cloned()
        .unwrap_or_else(|| wanted.to_owned())
}

fn input_locale(layout: &str, variant: Option<&str>) -> Option<String> {
    let lookup = |key: &str| {
        KEYBOARDS
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, klid)| (*klid).to_owned())
    };
    variant
        .and_then(|variant| lookup(&format!("{layout}({variant})")))
        .or_else(|| lookup(layout))
}

/// The first configured XKB layout and its variant, from the files
/// `localectl` and the Debian keyboard configuration keep.
fn keyboard_layout() -> Option<(String, Option<String>)> {
    let first = |value: &str| {
        value
            .split(',')
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    if let Ok(text) = std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf") {
        let option = |name: &str| {
            text.lines().find_map(|line| {
                let rest = line.trim().strip_prefix("Option")?.trim();
                let rest = rest.strip_prefix(&format!("\"{name}\""))?.trim();
                first(rest.trim_matches('"'))
            })
        };
        if let Some(layout) = option("XkbLayout") {
            return Some((layout, option("XkbVariant")));
        }
    }
    for path in ["/etc/default/keyboard", "/etc/vconsole.conf"] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let value = |key: &str| {
            text.lines().find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name.trim() == key).then(|| first(value.trim().trim_matches('"')))?
            })
        };
        if let Some(layout) = value("XKBLAYOUT").or_else(|| value("KEYMAP")) {
            return Some((layout, value("XKBVARIANT")));
        }
    }
    None
}

/// The IANA zone of this session: `TZ`, `/etc/timezone`, or the target of
/// the `/etc/localtime` link.
fn time_zone_name() -> Option<String> {
    if let Ok(zone) = std::env::var("TZ") {
        let zone = zone.trim_start_matches(':');
        if let Some(name) = zone_from_path(Path::new(zone)) {
            return Some(name);
        }
        if !zone.is_empty() && !zone.starts_with('/') {
            return Some(zone.to_owned());
        }
    }
    if let Ok(text) = std::fs::read_to_string("/etc/timezone") {
        let zone = text.trim();
        if !zone.is_empty() {
            return Some(zone.to_owned());
        }
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    zone_from_path(&target)
}

fn zone_from_path(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let (_, zone) = text.split_once("zoneinfo/")?;
    let zone = zone
        .strip_prefix("posix/")
        .or_else(|| zone.strip_prefix("right/"))
        .unwrap_or(zone);
    (!zone.is_empty()).then(|| zone.to_owned())
}

pub fn windows_zone(iana: &str) -> Option<String> {
    WINDOWS_ZONES
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (zone, windows) = line.split_once('\t')?;
            (zone == iana).then(|| windows.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_locales_become_windows_tags() {
        assert_eq!(windows_locale("en_PH.UTF-8").as_deref(), Some("en-PH"));
        assert_eq!(windows_locale("fil_PH").as_deref(), Some("fil-PH"));
        assert_eq!(windows_locale("de_DE.UTF-8@euro").as_deref(), Some("de-DE"));
        assert_eq!(windows_locale("C.UTF-8"), None);
        assert_eq!(windows_locale("POSIX"), None);
    }

    #[test]
    fn display_language_must_exist_in_the_image() {
        let image = vec!["en-US".to_owned(), "fr-FR".to_owned()];
        assert_eq!(ui_language("fr-FR", &image), "fr-FR");
        assert_eq!(ui_language("en-PH", &image), "en-US");
        assert_eq!(ui_language("fil-PH", &image), "en-US");
        assert_eq!(ui_language("de-DE", &[]), "de-DE");
    }

    #[test]
    fn keyboards_and_zones_map_to_windows_names() {
        assert_eq!(input_locale("us", None).as_deref(), Some("0409:00000409"));
        assert_eq!(
            input_locale("us", Some("intl")).as_deref(),
            Some("0409:00020409")
        );
        assert_eq!(
            input_locale("de", Some("nodeadkeys")).as_deref(),
            Some("0407:00000407")
        );
        assert_eq!(input_locale("xx", None), None);
        assert_eq!(
            windows_zone("Asia/Manila").as_deref(),
            Some("Singapore Standard Time")
        );
        assert_eq!(
            windows_zone("Europe/Berlin").as_deref(),
            Some("W. Europe Standard Time")
        );
        assert_eq!(
            windows_zone("Asia/Kolkata").as_deref(),
            Some("India Standard Time")
        );
        assert_eq!(windows_zone("Etc/UTC").as_deref(), Some("UTC"));
        assert_eq!(windows_zone("Mars/Olympus"), None);
        assert_eq!(
            zone_from_path(Path::new("/usr/share/zoneinfo/America/New_York")).as_deref(),
            Some("America/New_York")
        );
        assert_eq!(
            zone_from_path(Path::new("../usr/share/zoneinfo/posix/Europe/Paris")).as_deref(),
            Some("Europe/Paris")
        );
    }

    #[test]
    fn every_value_passes_the_protocol_rules() {
        let settings = current(&["en-US".to_owned()]);
        let customization = rufus_helper_protocol::WindowsCustomization {
            regional: Some(settings),
            ..Default::default()
        };
        customization.validate().expect("valid regional settings");
        for line in WINDOWS_ZONES.lines().filter(|line| !line.starts_with('#')) {
            let (_, windows) = line.split_once('\t').expect("tab-separated");
            let customization = rufus_helper_protocol::WindowsCustomization {
                regional: Some(RegionalSettings {
                    input_locale: "en-US".into(),
                    system_locale: "en-US".into(),
                    user_locale: "en-US".into(),
                    ui_language: "en-US".into(),
                    time_zone: Some(windows.into()),
                }),
                ..Default::default()
            };
            customization
                .validate()
                .unwrap_or_else(|_| panic!("{windows} is rejected"));
        }
    }
}
