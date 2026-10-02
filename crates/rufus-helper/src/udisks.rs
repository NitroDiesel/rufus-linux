//! Minimal blocking client for the system udisks2 daemon.
//!
//! udisks2 performs the privileged work (partitioning, mkfs, mounting, opening
//! the raw device) on behalf of the logged-in user under its own polkit
//! policy, so the desktop needs no helper of its own.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::PathBuf;

use zbus::blocking::{Connection, Proxy, ProxyBuilder};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::HelperError;

const SERVICE: &str = "org.freedesktop.UDisks2";
const MANAGER: &str = "/org/freedesktop/UDisks2/Manager";
const MANAGER_IFACE: &str = "org.freedesktop.UDisks2.Manager";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const PARTITION: &str = "org.freedesktop.UDisks2.Partition";
const PARTITION_TABLE: &str = "org.freedesktop.UDisks2.PartitionTable";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";

pub type ObjectPath = OwnedObjectPath;
type Options<'a> = HashMap<&'a str, Value<'a>>;

pub struct Udisks {
    connection: Connection,
}

/// Geometry of a partition as recorded in the partition table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartitionGeometry {
    pub number: u32,
    pub offset: u64,
    pub size: u64,
}

impl Udisks {
    pub fn connect() -> Result<Self, HelperError> {
        let connection = Connection::system().map_err(|error| {
            HelperError::Operation(format!("could not reach the system bus: {error}"))
        })?;
        let udisks = Self { connection };
        udisks.version()?;
        Ok(udisks)
    }

    fn proxy(&self, path: &str, interface: &'static str) -> Result<Proxy<'static>, HelperError> {
        ProxyBuilder::new(&self.connection)
            .destination(SERVICE)
            .and_then(|builder| builder.path(path.to_owned()))
            .and_then(|builder| builder.interface(interface))
            .map(|builder| builder.cache_properties(CacheProperties::No))
            .and_then(|builder| builder.build())
            .map_err(|error| dbus_error("udisks2", error))
    }

    fn property<T>(&self, path: &str, interface: &'static str, name: &str) -> Result<T, HelperError>
    where
        T: TryFrom<zbus::zvariant::OwnedValue>,
        T::Error: Into<zbus::Error>,
    {
        self.proxy(path, interface)?
            .get_property(name)
            .map_err(|error| dbus_error(name, error))
    }

    fn has_interface(&self, path: &str, interface: &'static str) -> Result<bool, HelperError> {
        let proxy = self.proxy(path, "org.freedesktop.DBus.Introspectable")?;
        let xml: String = proxy
            .call("Introspect", &())
            .map_err(|error| dbus_error("Introspect", error))?;
        Ok(xml.contains(&format!("<interface name=\"{interface}\"")))
    }

    pub fn version(&self) -> Result<String, HelperError> {
        self.property(MANAGER, MANAGER_IFACE, "Version")
            .map_err(|_| HelperError::Operation("the udisks2 service is not available".into()))
    }

    /// udisks 2.10 added `mkfs-args`; older daemons ignore unknown options.
    pub fn supports_mkfs_args(&self) -> Result<bool, HelperError> {
        Ok(version_at_least(&self.version()?, 2, 10))
    }

    pub fn can_format(&self, kind: &str) -> Result<Result<(), String>, HelperError> {
        let (available, missing): (bool, String) = self
            .proxy(MANAGER, MANAGER_IFACE)?
            .call("CanFormat", &(kind,))
            .map_err(|error| dbus_error("CanFormat", error))?;
        Ok(if available { Ok(()) } else { Err(missing) })
    }

    pub fn block_for_device(&self, major: u32, minor: u32) -> Result<ObjectPath, HelperError> {
        let wanted = libc::makedev(major, minor);
        let devices: Vec<OwnedObjectPath> = self
            .proxy(MANAGER, MANAGER_IFACE)?
            .call("GetBlockDevices", &(Options::new(),))
            .map_err(|error| dbus_error("GetBlockDevices", error))?;
        for path in devices {
            let number: u64 = self.property(path.as_str(), BLOCK, "DeviceNumber")?;
            if number == wanted {
                return Ok(path);
            }
        }
        Err(HelperError::Revalidation(format!(
            "udisks2 does not know device {major}:{minor}"
        )))
    }

    pub fn size(&self, block: &ObjectPath) -> Result<u64, HelperError> {
        self.property(block.as_str(), BLOCK, "Size")
    }

    pub fn read_only(&self, block: &ObjectPath) -> Result<bool, HelperError> {
        self.property(block.as_str(), BLOCK, "ReadOnly")
    }

    pub fn drive_serial(&self, block: &ObjectPath) -> Result<String, HelperError> {
        let drive: OwnedObjectPath = self.property(block.as_str(), BLOCK, "Drive")?;
        if drive.as_str() == "/" {
            return Ok(String::new());
        }
        self.property(drive.as_str(), DRIVE, "Serial")
    }

    pub fn partitions(&self, disk: &ObjectPath) -> Result<Vec<ObjectPath>, HelperError> {
        if !self.has_interface(disk.as_str(), PARTITION_TABLE)? {
            return Ok(Vec::new());
        }
        self.property(disk.as_str(), PARTITION_TABLE, "Partitions")
    }

    pub fn partition_geometry(&self, part: &ObjectPath) -> Result<PartitionGeometry, HelperError> {
        Ok(PartitionGeometry {
            number: self.property(part.as_str(), PARTITION, "Number")?,
            offset: self.property(part.as_str(), PARTITION, "Offset")?,
            size: self.property(part.as_str(), PARTITION, "Size")?,
        })
    }

    pub fn mount_points(&self, block: &ObjectPath) -> Result<Vec<PathBuf>, HelperError> {
        if !self.has_interface(block.as_str(), FILESYSTEM)? {
            return Ok(Vec::new());
        }
        let points: Vec<Vec<u8>> = self.property(block.as_str(), FILESYSTEM, "MountPoints")?;
        Ok(points
            .iter()
            .map(|raw| PathBuf::from(bytes_to_string(raw)))
            .collect())
    }

    pub fn unmount(&self, block: &ObjectPath) -> Result<(), HelperError> {
        self.proxy(block.as_str(), FILESYSTEM)?
            .call::<_, _, ()>("Unmount", &(Options::new(),))
            .map_err(|error| dbus_error("unmount", error))
    }

    /// Mount through udisks2; the mount belongs to the calling user.
    pub fn mount(&self, block: &ObjectPath, options: &str) -> Result<PathBuf, HelperError> {
        let mut arguments = Options::new();
        if !options.is_empty() {
            arguments.insert("options", Value::from(options));
        }
        let path: String = self
            .proxy(block.as_str(), FILESYSTEM)?
            .call("Mount", &(arguments,))
            .map_err(|error| dbus_error("mount", error))?;
        Ok(PathBuf::from(path))
    }

    /// Create a partition table (`gpt`/`dos`) or filesystem on a block device.
    pub fn format(
        &self,
        block: &ObjectPath,
        kind: &str,
        label: Option<&str>,
        mkfs_args: &[String],
        zero_first: bool,
        take_ownership: bool,
    ) -> Result<(), HelperError> {
        let mut options = Options::new();
        if let Some(label) = label {
            options.insert("label", Value::from(label));
        }
        if !mkfs_args.is_empty() {
            options.insert("mkfs-args", Value::from(mkfs_args.to_vec()));
        }
        if zero_first {
            options.insert("erase", Value::from("zero"));
        }
        if take_ownership {
            options.insert("take-ownership", Value::from(true));
        }
        self.proxy(block.as_str(), BLOCK)?
            .call::<_, _, ()>("Format", &(kind, options))
            .map_err(|error| dbus_error(&format!("format as {kind}"), error))
    }

    pub fn create_partition(
        &self,
        disk: &ObjectPath,
        offset: u64,
        size: u64,
        partition_type: &str,
        name: &str,
    ) -> Result<ObjectPath, HelperError> {
        self.proxy(disk.as_str(), PARTITION_TABLE)?
            .call(
                "CreatePartition",
                &(offset, size, partition_type, name, Options::new()),
            )
            .map_err(|error| dbus_error("create partition", error))
    }

    /// GPT: the 64-bit attribute field. MBR: 0x80 marks the active partition.
    pub fn set_partition_flags(&self, part: &ObjectPath, flags: u64) -> Result<(), HelperError> {
        self.proxy(part.as_str(), PARTITION)?
            .call::<_, _, ()>("SetFlags", &(flags, Options::new()))
            .map_err(|error| dbus_error("set partition flags", error))
    }

    /// Open the device through udisks2. This is the one step that asks for
    /// an administrator password (`org.freedesktop.udisks2.open-device`).
    pub fn open(&self, block: &ObjectPath, mode: &str, flags: i32) -> Result<File, HelperError> {
        let mut options = Options::new();
        options.insert("flags", Value::from(flags));
        let fd: zbus::zvariant::OwnedFd = self
            .proxy(block.as_str(), BLOCK)?
            .call("OpenDevice", &(mode, options))
            .map_err(|error| dbus_error("open device", error))?;
        Ok(File::from(OwnedFd::from(fd)))
    }

    pub fn rescan(&self, block: &ObjectPath) -> Result<(), HelperError> {
        self.proxy(block.as_str(), BLOCK)?
            .call::<_, _, ()>("Rescan", &(Options::new(),))
            .map_err(|error| dbus_error("rescan", error))
    }

    /// Attach `file` as a loop device owned by the caller.
    #[cfg(test)]
    pub fn loop_setup(&self, file: &File) -> Result<ObjectPath, HelperError> {
        use std::os::fd::AsFd;
        let fd = zbus::zvariant::Fd::from(file.as_fd());
        self.proxy(MANAGER, MANAGER_IFACE)?
            .call("LoopSetup", &(fd, Options::new()))
            .map_err(|error| dbus_error("loop setup", error))
    }

    #[cfg(test)]
    pub fn device_number(&self, block: &ObjectPath) -> Result<u64, HelperError> {
        self.property(block.as_str(), BLOCK, "DeviceNumber")
    }

    #[cfg(test)]
    pub fn partition_property<T>(&self, part: &ObjectPath, name: &str) -> Result<T, HelperError>
    where
        T: TryFrom<zbus::zvariant::OwnedValue>,
        T::Error: Into<zbus::Error>,
    {
        self.property(part.as_str(), PARTITION, name)
    }

    #[cfg(test)]
    pub fn loop_delete(&self, block: &ObjectPath) -> Result<(), HelperError> {
        self.proxy(block.as_str(), "org.freedesktop.UDisks2.Loop")?
            .call::<_, _, ()>("Delete", &(Options::new(),))
            .map_err(|error| dbus_error("loop delete", error))
    }
}

fn bytes_to_string(raw: &[u8]) -> String {
    let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

fn version_at_least(version: &str, major: u32, minor: u32) -> bool {
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let found = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    found >= (major, minor)
}

fn dbus_error(action: &str, error: zbus::Error) -> HelperError {
    if let zbus::Error::MethodError(name, detail, _) = &error {
        let name = name.as_str();
        let detail = detail.clone().unwrap_or_default();
        if name.ends_with("NotAuthorizedDismissed") {
            return HelperError::Cancelled;
        }
        if name.contains("NotAuthorized") {
            return HelperError::Operation(format!(
                "{action}: authorization was refused ({detail})"
            ));
        }
        return HelperError::Operation(format!("{action}: {detail}"));
    }
    HelperError::Operation(format!("{action}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(version_at_least("2.10.1", 2, 10));
        assert!(version_at_least("2.11.0", 2, 10));
        assert!(!version_at_least("2.9.4", 2, 10));
        assert!(version_at_least("3.0", 2, 10));
    }

    #[test]
    fn device_bytes_stop_at_nul() {
        assert_eq!(bytes_to_string(b"/dev/sdb\0"), "/dev/sdb");
        assert_eq!(bytes_to_string(b"/dev/sdb"), "/dev/sdb");
    }
}
