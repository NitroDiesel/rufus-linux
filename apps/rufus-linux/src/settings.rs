//! Preferences kept across launches in
//! `$XDG_CONFIG_HOME/rufus-linux/settings.conf` as `key=value` lines.

use std::path::PathBuf;

use crate::units::{SizeUnit, SpeedUnit};
use crate::wue::WueOption;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub size_unit: SizeUnit,
    pub speed_unit: SpeedUnit,
    /// Windows User Experience choices, like upstream's `WUEOptions`.
    pub wue_options: Vec<WueOption>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            size_unit: SizeUnit::Auto,
            speed_unit: SpeedUnit::MBps,
            wue_options: crate::wue::default_selection(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Best effort: an unwritable config directory only loses the preference.
    pub fn save(&self) {
        // Unit tests must not touch the developer's own settings.
        if cfg!(test) {
            return;
        }
        let Some(path) = path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, self.serialize());
    }

    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
            match key.trim() {
                "size_unit" => {
                    if let Some(unit) = SizeUnit::from_label(value.trim()) {
                        settings.size_unit = unit;
                    }
                }
                "speed_unit" => {
                    if let Some(unit) = SpeedUnit::from_label(value.trim()) {
                        settings.speed_unit = unit;
                    }
                }
                "wue_options" => {
                    settings.wue_options = value
                        .split(',')
                        .filter_map(|key| WueOption::from_key(key.trim()))
                        .collect();
                }
                _ => {}
            }
        }
        settings
    }

    fn serialize(&self) -> String {
        let wue: Vec<_> = crate::wue::remembered(&self.wue_options)
            .into_iter()
            .map(WueOption::key)
            .collect();
        format!(
            "size_unit={}\nspeed_unit={}\nwue_options={}\n",
            self.size_unit.label(),
            self.speed_unit.label(),
            wue.join(",")
        )
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("rufus-linux").join("settings.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ignores_unknown_or_invalid_lines() {
        let settings = Settings {
            size_unit: SizeUnit::Gb,
            speed_unit: SpeedUnit::Mbps,
            wue_options: vec![WueOption::LocalAccount, WueOption::QualityOfLife],
        };
        assert_eq!(Settings::parse(&settings.serialize()), settings);
        assert_eq!(
            Settings::parse("speed_unit=furlongs\nfuture=1\nsize_unit = MB\n"),
            Settings {
                size_unit: SizeUnit::Mb,
                ..Settings::default()
            }
        );
        // An empty list is a choice; silent install is never restored.
        assert!(Settings::parse("wue_options=\n").wue_options.is_empty());
        assert_eq!(
            Settings::parse("wue_options=silent,regional,bogus\n").wue_options,
            [WueOption::Regional]
        );
    }
}
