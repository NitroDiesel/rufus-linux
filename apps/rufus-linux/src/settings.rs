//! Display preferences kept across launches in
//! `$XDG_CONFIG_HOME/rufus-linux/settings.conf` as `key=value` lines.

use std::path::PathBuf;

use crate::units::{SizeUnit, SpeedUnit};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Settings {
    pub size_unit: SizeUnit,
    pub speed_unit: SpeedUnit,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            size_unit: SizeUnit::Auto,
            speed_unit: SpeedUnit::MBps,
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
                _ => {}
            }
        }
        settings
    }

    fn serialize(&self) -> String {
        format!(
            "size_unit={}\nspeed_unit={}\n",
            self.size_unit.label(),
            self.speed_unit.label()
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
        };
        assert_eq!(Settings::parse(&settings.serialize()), settings);
        assert_eq!(
            Settings::parse("speed_unit=furlongs\nfuture=1\nsize_unit = MB\n"),
            Settings {
                size_unit: SizeUnit::Mb,
                speed_unit: SpeedUnit::MBps,
            }
        );
    }
}
