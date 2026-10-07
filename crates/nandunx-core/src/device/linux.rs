//! Linux physical-device discovery and write barrier. No fixture writer calls this module.
use crate::{
    AuthorizedRestoreTarget, BlockDevice, RestoreDeviceError, RestoreTargetSafetyError,
    LOGICAL_SECTOR_BYTES,
};
use std::{
    env,
    fs::{self, File},
    io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

/// Returns user-visible physical block-device candidates from Linux sysfs.
/// This performs no I/O against the devices themselves.
pub fn list_block_devices() -> io::Result<Vec<BlockDevice>> {
    let mut devices = Vec::new();

    for entry in fs::read_dir("/sys/block")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_physical_device_name(&name) {
            continue;
        }

        let sysfs_path = entry.path();
        let sectors = read_sysfs_u64(sysfs_path.join("size")).unwrap_or(0);
        if sectors == 0 {
            continue;
        }
        let byte_len = sectors.saturating_mul(LOGICAL_SECTOR_BYTES);
        let model =
            read_trimmed(sysfs_path.join("device/model")).unwrap_or_else(|| name.to_string());
        let is_removable = read_sysfs_u64(sysfs_path.join("removable")).unwrap_or(0) != 0;
        let is_read_only = read_sysfs_u64(sysfs_path.join("ro")).unwrap_or(1) != 0;
        let boot_partitions_available = boot_partitions_available(&name);

        devices.push(BlockDevice {
            path: format!("/dev/{name}"),
            model,
            byte_len,
            is_removable,
            is_read_only,
            boot_partitions_available,
        });
    }

    devices.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(devices)
}

pub(crate) fn current_physical_block(
    selected: &BlockDevice,
) -> Result<(String, BlockDevice), RestoreTargetSafetyError> {
    let requested = fs::canonicalize(&selected.path)
        .map_err(|_| RestoreTargetSafetyError::TargetUnavailable)?;
    let requested = requested
        .to_str()
        .map(str::to_owned)
        .ok_or(RestoreTargetSafetyError::TargetUnavailable)?;
    let target = list_block_devices()
        .map_err(|_| RestoreTargetSafetyError::TargetUnavailable)?
        .into_iter()
        .find(|candidate| {
            fs::canonicalize(&candidate.path)
                .ok()
                .and_then(|path| path.to_str().map(str::to_owned))
                .as_deref()
                == Some(requested.as_str())
        })
        .ok_or(RestoreTargetSafetyError::TargetNotPhysicalDevice)?;
    if target.is_read_only {
        return Err(RestoreTargetSafetyError::TargetReadOnly);
    }
    if target.byte_len != selected.byte_len {
        return Err(RestoreTargetSafetyError::TargetChanged);
    }
    Ok((requested, target))
}

pub(crate) fn ensure_unmounted_target(
    canonical_path: &str,
) -> Result<(), RestoreTargetSafetyError> {
    let target_name = Path::new(canonical_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RestoreTargetSafetyError::TargetNotPhysicalDevice)?;
    if !mounted_at_for_target(target_name)
        .map_err(|_| RestoreTargetSafetyError::TargetUnavailable)?
        .is_empty()
    {
        return Err(RestoreTargetSafetyError::TargetMounted);
    }
    Ok(())
}

pub(crate) fn paths_name_same_device(left: &Path, right: &Path) -> bool {
    if fs::canonicalize(left)
        .ok()
        .zip(fs::canonicalize(right).ok())
        .is_some_and(|(left, right)| left == right)
    {
        return true;
    }
    let Ok(left) = fs::metadata(left) else {
        return false;
    };
    let Ok(right) = fs::metadata(right) else {
        return false;
    };
    left.file_type().is_block_device()
        && right.file_type().is_block_device()
        && left.rdev() == right.rdev()
}

pub(crate) fn mounted_at_for_target(target_name: &str) -> io::Result<Vec<String>> {
    // A container has its own mount namespace. A caller that deliberately
    // runs this service in one can provide a read-only bind mount of the
    // host's mountinfo here, so the destructive target barrier still sees
    // mounts made by the host. The normal, non-container default remains
    // this process's mount namespace.
    let mountinfo = fs::read_to_string(mountinfo_path_from_environment(env::var_os(
        "NANDUNX_MOUNTINFO_PATH",
    )))?;
    let mut mount_points = Vec::new();
    for line in mountinfo.lines() {
        let Some((device, mount_point, source)) = parse_mountinfo_line(line) else {
            continue;
        };
        if mount_device_belongs_to_target(target_name, device)
            || source_mentions_target(source, target_name)
        {
            mount_points.push(unescape_mountinfo_path(mount_point));
        }
    }
    mount_points.sort();
    mount_points.dedup();
    Ok(mount_points)
}

pub(crate) fn mountinfo_path_from_environment(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/proc/self/mountinfo"))
}

/// Returns (major:minor, mount point, source) from one mountinfo record.
/// Mountinfo escapes whitespace and backslashes as octal values; decoding is
/// postponed until a matching mount is reported to the user.
pub(crate) fn parse_mountinfo_line(line: &str) -> Option<(&str, &str, &str)> {
    let (before_separator, after_separator) = line.split_once(" - ")?;
    let mut fields = before_separator.split_whitespace();
    fields.next()?; // mount ID
    fields.next()?; // parent ID
    let device = fields.next()?;
    fields.next()?; // root
    let mount_point = fields.next()?;
    let source = after_separator.split_whitespace().nth(1)?;
    Some((device, mount_point, source))
}

fn mount_device_belongs_to_target(target_name: &str, device: &str) -> bool {
    let sysfs = Path::new("/sys/dev/block").join(device);
    let Ok(resolved) = fs::canonicalize(sysfs) else {
        return false;
    };
    block_node_has_target(&resolved, target_name, 0)
}

/// Device-mapper volumes do not retain the physical disk name in their own
/// mountinfo source. Follow their sysfs `slaves` links so a mounted
/// `/dev/mapper/...` volume still protects its backing physical target.
fn block_node_has_target(path: &Path, target_name: &str, depth: u8) -> bool {
    if depth > 16 {
        return true; // An unexpectedly deep mapping is unsafe to ignore.
    }
    if sysfs_parent_block_name(path) == Some(target_name) {
        return true;
    }
    let Ok(slaves) = fs::read_dir(path.join("slaves")) else {
        return false;
    };
    slaves.filter_map(Result::ok).any(|entry| {
        fs::canonicalize(entry.path())
            .ok()
            .is_some_and(|slave| block_node_has_target(&slave, target_name, depth + 1))
    })
}

pub(crate) fn sysfs_parent_block_name(path: &Path) -> Option<&str> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == "block" {
            return components
                .next()
                .and_then(|component| component.as_os_str().to_str());
        }
    }
    None
}

pub(crate) fn source_mentions_target(source: &str, target_name: &str) -> bool {
    let Some(name) = source.strip_prefix("/dev/") else {
        return false;
    };
    name == target_name
        || name.strip_prefix(target_name).is_some_and(|suffix| {
            suffix.starts_with('p')
                || suffix
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_digit())
        })
}

pub(crate) fn unescape_mountinfo_path(value: &str) -> String {
    value
        .replace(r"\040", " ")
        .replace(r"\011", "\t")
        .replace(r"\134", r"\")
}

pub(crate) fn is_physical_device_name(name: &str) -> bool {
    !(name.starts_with("loop") || name.starts_with("ram") || name.starts_with("zram"))
}

/// Linux exposes eMMC boot areas as separate nodes only for adapters/drivers
/// that support them. A missing pair does not block RAWNAND/USER work, but it
/// requires a separate BOOT0/BOOT1 restore in Hekate.
fn boot_partitions_available(name: &str) -> bool {
    name.starts_with("mmcblk")
        && Path::new(&format!("/dev/{name}boot0")).exists()
        && Path::new(&format!("/dev/{name}boot1")).exists()
}

fn read_sysfs_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) fn ensure_same_authorized_device(
    authorized: &AuthorizedRestoreTarget,
    writable: &File,
) -> Result<(), RestoreDeviceError> {
    let locked = authorized
        .file
        .metadata()
        .map_err(|_| RestoreDeviceError::TargetChanged)?;
    let writable = writable
        .metadata()
        .map_err(|_| RestoreDeviceError::TargetChanged)?;
    if !locked.file_type().is_block_device()
        || !writable.file_type().is_block_device()
        || locked.rdev() != writable.rdev()
    {
        return Err(RestoreDeviceError::TargetChanged);
    }
    Ok(())
}
