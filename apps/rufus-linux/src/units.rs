//! Display units for progress telemetry, chosen by the user in the Status card.

use rufus_core::progress::ProgressUnit;

/// Scale for transferred sizes. KB, MB and GB are decimal; KiB, MiB and GiB
/// are binary. Auto picks a binary unit, like the device list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizeUnit {
    Auto,
    Bytes,
    Kb,
    Kib,
    Mb,
    Mib,
    Gb,
    Gib,
}

impl SizeUnit {
    pub const ALL: [Self; 8] = [
        Self::Auto,
        Self::Bytes,
        Self::Kb,
        Self::Kib,
        Self::Mb,
        Self::Mib,
        Self::Gb,
        Self::Gib,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Bytes => "Bytes",
            Self::Kb => "KB",
            Self::Kib => "KiB",
            Self::Mb => "MB",
            Self::Mib => "MiB",
            Self::Gb => "GB",
            Self::Gib => "GiB",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|unit| unit.label() == label)
    }

    fn fixed_for(self, bytes: u64) -> Self {
        if self != Self::Auto {
            return self;
        }
        match bytes {
            0..KIB => Self::Bytes,
            KIB..MIB => Self::Kib,
            MIB..GIB => Self::Mib,
            _ => Self::Gib,
        }
    }

    fn divisor(self) -> u64 {
        match self {
            Self::Auto | Self::Bytes => 1,
            Self::Kb => 1_000,
            Self::Mb => 1_000_000,
            Self::Gb => 1_000_000_000,
            Self::Kib => KIB,
            Self::Mib => MIB,
            Self::Gib => GIB,
        }
    }
}

/// Transfer rate unit. Byte rates are binary like sizes; bit rates are
/// decimal, as network speeds are quoted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpeedUnit {
    KBps,
    Kbps,
    MBps,
    Mbps,
    GBps,
    Gbps,
}

impl SpeedUnit {
    pub const ALL: [Self; 6] = [
        Self::KBps,
        Self::Kbps,
        Self::MBps,
        Self::Mbps,
        Self::GBps,
        Self::Gbps,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::KBps => "KB/s",
            Self::Kbps => "Kbps",
            Self::MBps => "MB/s",
            Self::Mbps => "Mbps",
            Self::GBps => "GB/s",
            Self::Gbps => "Gbps",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|unit| unit.label() == label)
    }

    fn convert(self, bytes_per_second: u64) -> f64 {
        let bytes = bytes_per_second as f64;
        match self {
            Self::KBps => bytes / KIB as f64,
            Self::MBps => bytes / MIB as f64,
            Self::GBps => bytes / GIB as f64,
            Self::Kbps => bytes * 8.0 / 1e3,
            Self::Mbps => bytes * 8.0 / 1e6,
            Self::Gbps => bytes * 8.0 / 1e9,
        }
    }
}

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;

/// One progress report, kept so a unit change re-renders immediately.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub unit: ProgressUnit,
    pub completed: u64,
    pub total: Option<u64>,
    pub bytes_per_second: Option<u64>,
}

pub fn telemetry(sample: &Sample, size: SizeUnit, speed: SpeedUnit) -> String {
    let Some(total) = sample.total else {
        return String::new();
    };
    let completed = sample.completed;
    match sample.unit {
        ProgressUnit::Bytes => {
            let mut parts = vec![size_pair(completed, total, size)];
            if let Some(rate) = sample.bytes_per_second.filter(|rate| *rate > 0) {
                parts.push(format!("{} {}", number(speed.convert(rate)), speed.label()));
                if total > completed {
                    parts.push(format!("{} left", duration((total - completed) / rate)));
                }
            }
            parts.join(" · ")
        }
        ProgressUnit::Steps => format!("Step {completed} of {total}"),
        ProgressUnit::Blocks => format!("{completed} / {total} blocks"),
        ProgressUnit::Files => format!("{completed} / {total} files"),
        ProgressUnit::Indeterminate => String::new(),
    }
}

fn size_pair(completed: u64, total: u64, size: SizeUnit) -> String {
    let done_unit = size.fixed_for(completed);
    let total_unit = size.fixed_for(total);
    let done = scaled(completed, done_unit);
    let all = scaled(total, total_unit);
    if done_unit == total_unit {
        format!("{done} / {all} {}", unit_suffix(total_unit))
    } else {
        format!(
            "{done} {} / {all} {}",
            unit_suffix(done_unit),
            unit_suffix(total_unit)
        )
    }
}

fn unit_suffix(unit: SizeUnit) -> &'static str {
    match unit {
        SizeUnit::Auto | SizeUnit::Bytes => "bytes",
        other => other.label(),
    }
}

fn scaled(bytes: u64, unit: SizeUnit) -> String {
    if unit.divisor() == 1 {
        return grouped(bytes);
    }
    number(bytes as f64 / unit.divisor() as f64)
}

/// Two decimals below 10, one below 1000, then whole numbers with separators.
fn number(value: f64) -> String {
    if value < 10.0 {
        format!("{value:.2}")
    } else if value < 1000.0 {
        format!("{value:.1}")
    } else {
        grouped(value.round() as u64)
    }
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn duration(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(completed: u64, total: u64, rate: Option<u64>) -> Sample {
        Sample {
            unit: ProgressUnit::Bytes,
            completed,
            total: Some(total),
            bytes_per_second: rate,
        }
    }

    #[test]
    fn auto_scales_each_side_and_adds_rate_and_time_left() {
        let sample = bytes(398_710_198, 9_041_688_765, Some(40 * MIB));
        assert_eq!(
            telemetry(&sample, SizeUnit::Auto, SpeedUnit::MBps),
            "380.2 MiB / 8.42 GiB · 40.0 MB/s · 3:26 left"
        );
    }

    #[test]
    fn fixed_size_units_share_one_suffix() {
        let sample = bytes(398_710_198, 9_041_688_765, None);
        assert_eq!(
            telemetry(&sample, SizeUnit::Gib, SpeedUnit::MBps),
            "0.37 / 8.42 GiB"
        );
        assert_eq!(
            telemetry(&sample, SizeUnit::Mib, SpeedUnit::MBps),
            "380.2 / 8,623 MiB"
        );
        assert_eq!(
            telemetry(&sample, SizeUnit::Gb, SpeedUnit::MBps),
            "0.40 / 9.04 GB"
        );
        assert_eq!(
            telemetry(&sample, SizeUnit::Mb, SpeedUnit::MBps),
            "398.7 / 9,042 MB"
        );
        assert_eq!(
            telemetry(&sample, SizeUnit::Kib, SpeedUnit::MBps),
            "389,365 / 8,829,774 KiB"
        );
        assert_eq!(
            telemetry(&sample, SizeUnit::Bytes, SpeedUnit::MBps),
            "398,710,198 / 9,041,688,765 bytes"
        );
    }

    #[test]
    fn bit_rates_are_decimal_and_byte_rates_binary() {
        let rate = 50_000_000;
        let sample = bytes(0, 4 * GIB, Some(rate));
        let speed = |unit| telemetry(&sample, SizeUnit::Auto, unit);
        assert!(speed(SpeedUnit::Mbps).contains("· 400.0 Mbps ·"));
        assert!(speed(SpeedUnit::Gbps).contains("· 0.40 Gbps ·"));
        assert!(speed(SpeedUnit::Kbps).contains("· 400,000 Kbps ·"));
        assert!(speed(SpeedUnit::MBps).contains("· 47.7 MB/s ·"));
        assert!(speed(SpeedUnit::KBps).contains("· 48,828 KB/s ·"));
        assert!(speed(SpeedUnit::GBps).contains("· 0.05 GB/s ·"));
    }

    #[test]
    fn long_jobs_show_hours_and_finished_jobs_omit_time_left() {
        let slow = bytes(0, 8 * GIB, Some(MIB));
        assert!(telemetry(&slow, SizeUnit::Auto, SpeedUnit::MBps).ends_with("2:16:32 left"));
        let done = bytes(GIB, GIB, Some(MIB));
        assert_eq!(
            telemetry(&done, SizeUnit::Auto, SpeedUnit::MBps),
            "1.00 / 1.00 GiB · 1.00 MB/s"
        );
    }

    #[test]
    fn non_byte_progress_reads_as_words() {
        let steps = Sample {
            unit: ProgressUnit::Steps,
            completed: 3,
            total: Some(7),
            bytes_per_second: None,
        };
        assert_eq!(
            telemetry(&steps, SizeUnit::Gb, SpeedUnit::Gbps),
            "Step 3 of 7"
        );
        let unknown = Sample {
            total: None,
            ..steps
        };
        assert_eq!(telemetry(&unknown, SizeUnit::Auto, SpeedUnit::MBps), "");
    }

    #[test]
    fn labels_round_trip() {
        for unit in SizeUnit::ALL {
            assert_eq!(SizeUnit::from_label(unit.label()), Some(unit));
        }
        for unit in SpeedUnit::ALL {
            assert_eq!(SpeedUnit::from_label(unit.label()), Some(unit));
        }
    }
}
