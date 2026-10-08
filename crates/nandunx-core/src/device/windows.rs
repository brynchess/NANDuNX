//! Windows disk inventory, read-only preflight and protected write handles.
//! The elevated desktop runs the writers through public core sessions.
use crate::{AuthorizedRestoreTarget, BlockDevice, RestoreDeviceError, RestoreTargetSafetyError};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::File,
    io,
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE,
        DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
        SP_DEVINFO_DATA,
    },
    Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_NO_MORE_FILES, ERROR_NO_MORE_ITEMS,
        ERROR_SHARING_VIOLATION, ERROR_WRITE_PROTECT, GENERIC_READ, GENERIC_WRITE, HANDLE,
        INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        BusTypeRAID, BusTypeSpaces, CreateFileW, FindClose, FindFirstVolumeW, FindNextVolumeW,
        GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, FILE_ATTRIBUTE_NORMAL,
        FILE_FLAG_WRITE_THROUGH, FILE_SHARE_READ, FILE_SHARE_WRITE,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
    },
    System::{
        Ioctl::{
            PropertyStandardQuery, StorageAccessAlignmentProperty, StorageDeviceProperty,
            DRIVE_LAYOUT_INFORMATION_EX, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME,
            GET_LENGTH_INFORMATION, GUID_DEVINTERFACE_DISK, IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
            IOCTL_DISK_GET_LENGTH_INFO, IOCTL_DISK_IS_WRITABLE, IOCTL_STORAGE_GET_DEVICE_NUMBER,
            IOCTL_STORAGE_QUERY_PROPERTY, PARTITION_INFORMATION_EX, PARTITION_STYLE_GPT,
            PARTITION_STYLE_MBR, STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR, STORAGE_DEVICE_DESCRIPTOR,
            STORAGE_DEVICE_NUMBER, STORAGE_PROPERTY_QUERY,
        },
        Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_MULTI_SZ},
        SystemInformation::GetWindowsDirectoryW,
        IO::DeviceIoControl,
    },
};

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct WindowsDiskSnapshot {
    pub device: BlockDevice,
    pub pnp_instance_id: String,
    pub serial_number: String,
    pub logical_sector_bytes: u32,
    pub physical_sector_bytes: u32,
    pub bus_type: i32,
    pub gpt_disk_id: Option<[u8; 16]>,
    pub dynamic_disk: Option<bool>,
    pub volumes: Vec<String>,
}

impl std::fmt::Debug for WindowsDiskSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsDiskSnapshot")
            .field("byte_len", &self.device.byte_len)
            .field("pnp_instance_id", &"[redacted]")
            .field("serial_number", &"[redacted]")
            .field("logical_sector_bytes", &self.logical_sector_bytes)
            .field("physical_sector_bytes", &self.physical_sector_bytes)
            .field("bus_type", &self.bus_type)
            .field("gpt_disk_id", &self.gpt_disk_id.is_some())
            .field("dynamic_disk", &self.dynamic_disk)
            .field("volume_count", &self.volumes.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowsPreflightError {
    Io,
    MissingIdentity,
    Changed,
    ChangedAt {
        step: &'static str,
        detail: String,
    },
    UnsupportedGeometry,
    UnsupportedDiskLayout,
    TargetReadOnly,
    SourceOnTarget,
    SystemDisk,
    PagefileDisk,
    AmbiguousVolume,
    ConfirmationMismatch,
    CannotProtect {
        step: &'static str,
        volume_index: Option<usize>,
        os_error: Option<i32>,
    },
}

impl std::fmt::Display for WindowsPreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Io => "Could not read the complete Windows disk and volume map.",
            Self::MissingIdentity => "The disk does not have an unambiguous hardware identity.",
            Self::Changed => "The disk changed after it was selected from the list.",
            Self::ChangedAt { .. } => "The disk changed after it was selected from the list.",
            Self::UnsupportedGeometry => "The disk does not have supported 512/512e geometry.",
            Self::UnsupportedDiskLayout => {
                "The disk has an unsupported LDM, RAID, or Storage Spaces layout."
            }
            Self::TargetReadOnly => "The target disk is read-only.",
            Self::SourceOnTarget => "The source or application file is on the target disk.",
            Self::SystemDisk => "The disk contains the Windows system.",
            Self::PagefileDisk => "The disk contains a page file, or that cannot be ruled out.",
            Self::AmbiguousVolume => "A volume cannot be unambiguously assigned to the disk.",
            Self::ConfirmationMismatch => "The confirmation must be the exact, full disk path.",
            Self::CannotProtect { .. } => {
                "Exclusive protection of the disk and its volumes could not be maintained."
            }
        };
        f.write_str(message)
    }
}

impl WindowsPreflightError {
    fn cannot_protect(step: &'static str, volume_index: Option<usize>, error: io::Error) -> Self {
        Self::CannotProtect {
            step,
            volume_index,
            os_error: error.raw_os_error(),
        }
    }
}

struct InfoSet(isize);
impl Drop for InfoSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
struct FindHandle(HANDLE);
impl Drop for FindHandle {
    fn drop(&mut self) {
        unsafe {
            FindClose(self.0);
        }
    }
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn utf16_z(value: &[u16]) -> String {
    OsString::from_wide(&value[..value.iter().position(|&c| c == 0).unwrap_or(value.len())])
        .to_string_lossy()
        .into_owned()
}
fn open_read(path: &OsStr) -> io::Result<File> {
    let path = wide(path);
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn open_direct(path: &OsStr, access: u32, flags: u32, sharing: u32) -> io::Result<File> {
    let path = wide(path);
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            sharing,
            null(),
            OPEN_EXISTING,
            flags,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}
fn ioctl(file: &File, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<usize> {
    let mut returned = 0;
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            if input.is_empty() {
                null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(returned as usize)
    }
}
fn read_struct<T: Copy>(bytes: &[u8]) -> io::Result<T> {
    if bytes.len() < size_of::<T>() {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(unsafe { (bytes.as_ptr() as *const T).read_unaligned() })
}
fn property(file: &File, property_id: i32, output: &mut [u8]) -> io::Result<usize> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: property_id,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let input = unsafe {
        std::slice::from_raw_parts(
            (&query as *const STORAGE_PROPERTY_QUERY).cast(),
            size_of::<STORAGE_PROPERTY_QUERY>(),
        )
    };
    ioctl(file, IOCTL_STORAGE_QUERY_PROPERTY, input, output)
}
fn descriptor_string(bytes: &[u8], offset: u32) -> String {
    let offset = offset as usize;
    if offset == 0 || offset >= bytes.len() {
        return String::new();
    }
    let end = bytes[offset..]
        .iter()
        .position(|&v| v == 0)
        .map(|v| offset + v)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[offset..end])
        .trim()
        .to_owned()
}

fn partition_layout(file: &File) -> io::Result<(Option<[u8; 16]>, bool)> {
    let mut bytes = vec![0u8; 64 * 1024];
    let len = ioctl(file, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, &[], &mut bytes)?;
    let header: DRIVE_LAYOUT_INFORMATION_EX = read_struct(&bytes[..len])?;
    let count = header.PartitionCount as usize;
    let offset = std::mem::offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
    let entry_len = size_of::<PARTITION_INFORMATION_EX>();
    if count > 1024 || len < offset + count * entry_len {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let disk_id = if header.PartitionStyle == PARTITION_STYLE_GPT as u32 {
        let guid = unsafe { header.Anonymous.Gpt.DiskId };
        Some(unsafe { std::mem::transmute::<_, [u8; 16]>(guid) })
    } else {
        None
    };
    let ldm_data = windows_sys::core::GUID::from_u128(0xaf9b60a0_1431_4f62_bc68_3311714a69ad);
    let ldm_metadata = windows_sys::core::GUID::from_u128(0x5808c8aa_7e8f_42e0_85d2_e1e90434cfb3);
    let mut dynamic = false;
    for index in 0..count {
        let at = offset + index * entry_len;
        let part: PARTITION_INFORMATION_EX = read_struct(&bytes[at..at + entry_len])?;
        if part.PartitionStyle == PARTITION_STYLE_MBR {
            dynamic |= unsafe { part.Anonymous.Mbr.PartitionType } == 0x42;
        } else if part.PartitionStyle == PARTITION_STYLE_GPT {
            let kind = unsafe { part.Anonymous.Gpt.PartitionType };
            let kind: [u8; 16] = unsafe { std::mem::transmute(kind) };
            dynamic |= kind == unsafe { std::mem::transmute::<_, [u8; 16]>(ldm_data) }
                || kind == unsafe { std::mem::transmute::<_, [u8; 16]>(ldm_metadata) };
        }
    }
    Ok((disk_id, dynamic))
}

fn disk_interfaces() -> io::Result<Vec<(OsString, String)>> {
    let info = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if info == -1 {
        return Err(io::Error::last_os_error());
    }
    let info = InfoSet(info);
    let mut result = Vec::new();
    for index in 0.. {
        let mut interface = SP_DEVICE_INTERFACE_DATA::default();
        interface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(
                info.0,
                null(),
                &GUID_DEVINTERFACE_DISK,
                index,
                &mut interface,
            )
        };
        if ok == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_NO_MORE_ITEMS as i32) {
                break;
            }
            return Err(err);
        }
        let mut needed = 0;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                info.0,
                &interface,
                null_mut(),
                0,
                &mut needed,
                null_mut(),
            );
        }
        if needed < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 || needed > 65536 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let mut detail = vec![0u64; (needed as usize).div_ceil(8)];
        let detail_ptr = detail
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        unsafe {
            (*detail_ptr).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        let mut devinfo = SP_DEVINFO_DATA::default();
        devinfo.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                info.0,
                &interface,
                detail_ptr,
                needed,
                &mut needed,
                &mut devinfo,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let offset = unsafe {
            (&(*detail_ptr).DevicePath as *const [u16; 1]) as usize - detail_ptr as usize
        };
        let chars = (needed as usize - offset) / 2;
        let path = unsafe {
            std::slice::from_raw_parts((detail_ptr as *const u8).add(offset).cast::<u16>(), chars)
        };
        let path = OsString::from_wide(
            &path[..path
                .iter()
                .position(|&c| c == 0)
                .ok_or(io::ErrorKind::InvalidData)?],
        );
        let mut id = vec![0u16; 4096];
        let mut id_len = 0;
        if unsafe {
            SetupDiGetDeviceInstanceIdW(
                info.0,
                &devinfo,
                id.as_mut_ptr(),
                id.len() as u32,
                &mut id_len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        result.push((path, utf16_z(&id)));
    }
    Ok(result)
}

fn read_disk(interface_path: &OsStr, pnp_instance_id: String) -> io::Result<WindowsDiskSnapshot> {
    let file = open_read(interface_path)?;
    let mut number = vec![0u8; size_of::<STORAGE_DEVICE_NUMBER>()];
    let len = ioctl(&file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut number)?;
    let number: STORAGE_DEVICE_NUMBER = read_struct(&number[..len])?;
    let path = format!(r"\\.\PhysicalDrive{}", number.DeviceNumber);
    let mut length = vec![0u8; size_of::<GET_LENGTH_INFORMATION>()];
    let len = ioctl(&file, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut length)?;
    let length: GET_LENGTH_INFORMATION = read_struct(&length[..len])?;
    if length.Length <= 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut alignment = vec![0u8; size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>()];
    let len = property(&file, StorageAccessAlignmentProperty, &mut alignment)?;
    let alignment: STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR = read_struct(&alignment[..len])?;
    let mut descriptor = vec![0u8; 4096];
    let len = property(&file, StorageDeviceProperty, &mut descriptor)?;
    let header: STORAGE_DEVICE_DESCRIPTOR = read_struct(&descriptor[..len])?;
    let is_read_only = match ioctl(&file, IOCTL_DISK_IS_WRITABLE, &[], &mut []) {
        Ok(_) => false,
        Err(error) if error.raw_os_error() == Some(ERROR_WRITE_PROTECT as i32) => true,
        Err(error) => return Err(error),
    };
    let serial = descriptor_string(&descriptor[..len], header.SerialNumberOffset);
    let model = descriptor_string(&descriptor[..len], header.ProductIdOffset);
    let layout = partition_layout(&file).ok();
    Ok(WindowsDiskSnapshot {
        device: BlockDevice {
            path,
            model,
            byte_len: length.Length as u64,
            is_removable: header.RemovableMedia,
            is_read_only,
            boot_partitions_available: false,
        },
        pnp_instance_id,
        serial_number: serial,
        logical_sector_bytes: alignment.BytesPerLogicalSector,
        physical_sector_bytes: alignment.BytesPerPhysicalSector,
        bus_type: header.BusType,
        gpt_disk_id: layout.as_ref().and_then(|(id, _)| *id),
        dynamic_disk: layout.map(|(_, dynamic)| dynamic),
        volumes: Vec::new(),
    })
}

fn disks() -> io::Result<Vec<WindowsDiskSnapshot>> {
    let mut disks = Vec::new();
    let mut numbers = BTreeSet::new();
    for (path, id) in disk_interfaces()? {
        let disk = read_disk(&path, id)?;
        if !numbers.insert(disk.device.path.clone()) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        disks.push(disk);
    }
    disks.sort_by(|a, b| a.device.path.cmp(&b.device.path));
    Ok(disks)
}

pub fn list_windows_disks() -> io::Result<Vec<WindowsDiskSnapshot>> {
    disks()
}

pub(crate) fn list_block_devices() -> io::Result<Vec<BlockDevice>> {
    Ok(disks()?.into_iter().map(|d| d.device).collect())
}

fn volume_names() -> io::Result<Vec<String>> {
    let mut buffer = vec![0u16; 1024];
    let find = unsafe { FindFirstVolumeW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if find == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let find = FindHandle(find);
    let mut names = vec![utf16_z(&buffer)];
    loop {
        buffer.fill(0);
        if unsafe { FindNextVolumeW(find.0, buffer.as_mut_ptr(), buffer.len() as u32) } == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(err);
        }
        names.push(utf16_z(&buffer));
    }
    Ok(names)
}

fn volume_disks(name: &str) -> io::Result<BTreeSet<u32>> {
    let path = name.trim_end_matches('\\');
    let file = open_read(OsStr::new(path))?;
    let mut buffer = vec![0u8; 64 * 1024];
    let len = match ioctl(
        &file,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        &[],
        &mut buffer,
    ) {
        Ok(len) => len,
        Err(error) if matches!(error.raw_os_error(), Some(code) if code == ERROR_MORE_DATA as i32 || code == ERROR_INSUFFICIENT_BUFFER as i32) =>
        {
            // The fixed upper bound is intentional: an unexpectedly complex
            // Storage Spaces/dynamic mapping is not a write candidate.
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Err(error) => return Err(error),
    };
    if len < 4 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let count = u32::from_ne_bytes(buffer[..4].try_into().unwrap()) as usize;
    // DISK_EXTENT starts at offset 8 due to its i64 alignment.
    let entry_size = size_of::<windows_sys::Win32::System::Ioctl::DISK_EXTENT>();
    if count == 0 || count > 1024 || len < 8 + count * entry_size {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut result = BTreeSet::new();
    for index in 0..count {
        let at = 8 + index * entry_size;
        let extent: windows_sys::Win32::System::Ioctl::DISK_EXTENT =
            read_struct(&buffer[at..at + entry_size])?;
        result.insert(extent.DiskNumber);
    }
    Ok(result)
}

fn volume_for_path(path: &Path) -> io::Result<String> {
    let source = wide(path.as_os_str());
    let mut root = vec![0u16; 32768];
    if unsafe { GetVolumePathNameW(source.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut name = vec![0u16; 1024];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(utf16_z(&name))
}

fn disk_number(path: &str) -> Option<u32> {
    path.strip_prefix(r"\\.\PhysicalDrive")?.parse().ok()
}

fn validate_snapshot(
    selected: &WindowsDiskSnapshot,
    current: &WindowsDiskSnapshot,
    volume_map: &BTreeMap<String, BTreeSet<u32>>,
    sources: &[String],
    system_volume: &str,
    pagefile_volumes: &[String],
) -> Result<(), WindowsPreflightError> {
    if selected.device != current.device
        || selected.pnp_instance_id != current.pnp_instance_id
        || selected.serial_number != current.serial_number
        || selected.logical_sector_bytes != current.logical_sector_bytes
        || selected.physical_sector_bytes != current.physical_sector_bytes
        || selected.bus_type != current.bus_type
        || selected.gpt_disk_id != current.gpt_disk_id
        || selected.dynamic_disk != current.dynamic_disk
    {
        return Err(WindowsPreflightError::Changed);
    }
    if current.pnp_instance_id.is_empty()
        || current.serial_number.is_empty()
        || current.gpt_disk_id.is_none_or(|id| id == [0; 16])
    {
        return Err(WindowsPreflightError::MissingIdentity);
    }
    if current.device.is_read_only {
        return Err(WindowsPreflightError::TargetReadOnly);
    }
    if current.logical_sector_bytes != 512
        || !matches!(current.physical_sector_bytes, 512 | 4096)
        || current.device.byte_len % 512 != 0
    {
        return Err(WindowsPreflightError::UnsupportedGeometry);
    }
    if current.bus_type == BusTypeRAID
        || current.bus_type == BusTypeSpaces
        || current.dynamic_disk != Some(false)
    {
        return Err(WindowsPreflightError::UnsupportedDiskLayout);
    }
    let number = disk_number(&current.device.path).ok_or(WindowsPreflightError::Changed)?;
    let touches = |name: &str| -> Result<bool, WindowsPreflightError> {
        let disks = volume_map
            .get(name)
            .ok_or(WindowsPreflightError::AmbiguousVolume)?;
        if disks.len() != 1 {
            return Err(WindowsPreflightError::AmbiguousVolume);
        }
        Ok(disks.contains(&number))
    };
    if touches(system_volume)? {
        return Err(WindowsPreflightError::SystemDisk);
    }
    for volume in pagefile_volumes {
        if touches(volume)? {
            return Err(WindowsPreflightError::PagefileDisk);
        }
    }
    for volume in sources {
        if touches(volume)? {
            return Err(WindowsPreflightError::SourceOnTarget);
        }
    }
    Ok(())
}

/// Reads the complete Windows volume map and compares a session snapshot.
/// `source_paths` must include every backup part, keyset, BOOT file and process
/// executable. It never opens the target for writing or authorizes a write.
pub fn inspect_windows_disk(
    selected: &WindowsDiskSnapshot,
    source_paths: &[PathBuf],
) -> Result<WindowsDiskSnapshot, WindowsPreflightError> {
    let mut current = disks()
        .map_err(|_| WindowsPreflightError::Io)?
        .into_iter()
        .find(|d| d.device.path == selected.device.path)
        .ok_or(WindowsPreflightError::Changed)?;
    let mut map = BTreeMap::new();
    for name in volume_names().map_err(|_| WindowsPreflightError::Io)? {
        let disks = volume_disks(&name).map_err(|_| WindowsPreflightError::AmbiguousVolume)?;
        map.insert(name, disks);
    }
    current.volumes = map
        .iter()
        .filter(|(_, ds)| disk_number(&current.device.path).is_some_and(|n| ds.contains(&n)))
        .map(|(name, _)| name.clone())
        .collect();
    let mut all_sources = source_paths.to_vec();
    all_sources.push(std::env::current_exe().map_err(|_| WindowsPreflightError::Io)?);
    let sources = all_sources
        .iter()
        .map(|p| {
            p.canonicalize()
                .and_then(|canonical| volume_for_path(&canonical))
                .map_err(|_| WindowsPreflightError::AmbiguousVolume)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut system_path = vec![0u16; 32768];
    let system_len =
        unsafe { GetWindowsDirectoryW(system_path.as_mut_ptr(), system_path.len() as u32) }
            as usize;
    if system_len == 0 || system_len >= system_path.len() {
        return Err(WindowsPreflightError::Io);
    }
    let system_path = PathBuf::from(OsString::from_wide(&system_path[..system_len]));
    let system_volume =
        volume_for_path(&system_path).map_err(|_| WindowsPreflightError::AmbiguousVolume)?;
    let pagefile_volumes = pagefile_volumes().map_err(|_| WindowsPreflightError::PagefileDisk)?;
    validate_snapshot(
        selected,
        &current,
        &map,
        &sources,
        &system_volume,
        &pagefile_volumes,
    )?;
    Ok(current)
}

/// Holds all target volumes locked for the lifetime of a direct disk handle.
/// There is deliberately no public write method: core sessions keep the
/// protected handle throughout authorization, writing and verification.
pub struct WindowsProtectedDisk {
    disk: File,
    _locked_volumes: Vec<File>,
    snapshot: WindowsDiskSnapshot,
}

impl WindowsProtectedDisk {
    pub(crate) fn target_mut(&mut self) -> &mut File {
        &mut self.disk
    }
    pub fn snapshot(&self) -> &WindowsDiskSnapshot {
        &self.snapshot
    }

    /// Flushes the same direct handle kept under volume protection.
    pub fn flush(&self) -> io::Result<()> {
        self.disk.sync_all()
    }
}

fn verify_direct_handle(file: &File, expected: &WindowsDiskSnapshot) -> io::Result<()> {
    let mut number = vec![0u8; size_of::<STORAGE_DEVICE_NUMBER>()];
    let len = ioctl(file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut number)?;
    let number: STORAGE_DEVICE_NUMBER = read_struct(&number[..len])?;
    if format!(r"\\.\PhysicalDrive{}", number.DeviceNumber) != expected.device.path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "disk number mismatch",
        ));
    }
    let mut length = vec![0u8; size_of::<GET_LENGTH_INFORMATION>()];
    let len = ioctl(file, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut length)?;
    let length: GET_LENGTH_INFORMATION = read_struct(&length[..len])?;
    if length.Length < 0 || length.Length as u64 != expected.device.byte_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "disk length mismatch",
        ));
    }
    let mut alignment = vec![0u8; size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>()];
    let len = property(file, StorageAccessAlignmentProperty, &mut alignment)?;
    let alignment: STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR = read_struct(&alignment[..len])?;
    if alignment.BytesPerLogicalSector != expected.logical_sector_bytes
        || alignment.BytesPerPhysicalSector != expected.physical_sector_bytes
        || partition_layout(file)?.0 != expected.gpt_disk_id
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "disk alignment or GPT ID mismatch",
        ));
    }
    Ok(())
}

/// Rechecks every source and the target, then locks every target volume before
/// dismounting any of them. A disk with no visible volume has no equivalent
/// protection here and is refused. The direct handle is never exposed.
pub fn protect_windows_disk(
    selected: &WindowsDiskSnapshot,
    source_paths: &[PathBuf],
    typed_confirmation: &str,
) -> Result<WindowsProtectedDisk, WindowsPreflightError> {
    if typed_confirmation != selected.device.path {
        return Err(WindowsPreflightError::ConfirmationMismatch);
    }
    let current = inspect_windows_disk(selected, source_paths)?;
    protect_inspected_disk(current)
}

fn protect_inspected_disk(
    current: WindowsDiskSnapshot,
) -> Result<WindowsProtectedDisk, WindowsPreflightError> {
    let no_volumes = current.volumes.is_empty();
    let number = disk_number(&current.device.path).ok_or(WindowsPreflightError::Changed)?;
    let mut locks = Vec::with_capacity(current.volumes.len());
    for (index, name) in current.volumes.iter().enumerate() {
        let path = name.trim_end_matches('\\');
        let volume = open_direct(
            OsStr::new(path),
            GENERIC_READ | GENERIC_WRITE,
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )
        .map_err(|error| {
            WindowsPreflightError::cannot_protect("otwarcie woluminu", Some(index + 1), error)
        })?;
        if volume_disks(name).map_err(|error| {
            WindowsPreflightError::cannot_protect(
                "odczyt extentów woluminu",
                Some(index + 1),
                error,
            )
        })? != BTreeSet::from([number])
        {
            return Err(WindowsPreflightError::Changed);
        }
        ioctl(&volume, FSCTL_LOCK_VOLUME, &[], &mut []).map_err(|error| {
            WindowsPreflightError::cannot_protect("blokada woluminu", Some(index + 1), error)
        })?;
        locks.push(volume);
    }
    // Only after every lock succeeded may a filesystem be dismounted.
    for (index, volume) in locks.iter().enumerate() {
        ioctl(volume, FSCTL_DISMOUNT_VOLUME, &[], &mut []).map_err(|error| {
            WindowsPreflightError::cannot_protect("odmontowanie woluminu", Some(index + 1), error)
        })?;
    }
    let disk = open_direct(
        OsStr::new(&current.device.path),
        GENERIC_READ | GENERIC_WRITE,
        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
        0,
    )
    .map_err(|error| {
        WindowsPreflightError::cannot_protect("otwarcie fizycznego dysku RW", None, error)
    })?;
    verify_direct_handle(&disk, &current).map_err(|error| WindowsPreflightError::ChangedAt {
        step: "verify direct handle",
        detail: error.to_string(),
    })?;
    if no_volumes {
        // No FSCTL lock is available for an unrecognised Switch layout.
        // Confirm this adapter enforces the non-sharing physical handle and
        // that Windows still has no volume to mount on the target.
        match open_read(OsStr::new(&current.device.path)) {
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => {}
            Err(error) => {
                return Err(WindowsPreflightError::cannot_protect(
                    "exclusive physical handle probe",
                    None,
                    error,
                ));
            }
            Ok(_) => {
                return Err(WindowsPreflightError::CannotProtect {
                    step: "physical handle permits concurrent access",
                    volume_index: None,
                    os_error: None,
                });
            }
        }
        for name in volume_names().map_err(|error| {
            WindowsPreflightError::cannot_protect("volume enumeration under lock", None, error)
        })? {
            if volume_disks(&name)
                .map_err(|error| {
                    WindowsPreflightError::cannot_protect("volume extents under lock", None, error)
                })?
                .contains(&number)
            {
                return Err(WindowsPreflightError::CannotProtect {
                    step: "target volume appeared under lock",
                    volume_index: None,
                    os_error: None,
                });
            }
        }
    }
    // PnP/serial are properties of the interface, not the PhysicalDrive
    // handle. Re-enumerate while the protected handle is held.
    let again = disks()
        .map_err(|error| WindowsPreflightError::ChangedAt {
            step: "re-enumerate disk under lock",
            detail: error.to_string(),
        })?
        .into_iter()
        .find(|item| item.device.path == current.device.path)
        .ok_or(WindowsPreflightError::ChangedAt {
            step: "disk absent under lock",
            detail: String::new(),
        })?;
    if again.device != current.device
        || again.pnp_instance_id != current.pnp_instance_id
        || again.serial_number != current.serial_number
        || again.gpt_disk_id != current.gpt_disk_id
    {
        return Err(WindowsPreflightError::ChangedAt {
            step: "disk identity changed under lock",
            detail: String::new(),
        });
    }
    Ok(WindowsProtectedDisk {
        disk,
        _locked_volumes: locks,
        snapshot: current,
    })
}

fn pagefile_volumes() -> io::Result<Vec<String>> {
    // ExistingPageFiles contains the paths active at this boot. The registry
    // query is read-only; an absent/malformed value fails the preflight.
    let key = wide(OsStr::new(
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management",
    ));
    let value = wide(OsStr::new("ExistingPageFiles"));
    let mut bytes = vec![0u8; 64 * 1024];
    let mut length = bytes.len() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_MULTI_SZ,
            null_mut(),
            bytes.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if length as usize % 2 != 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let words = bytes[..length as usize]
        .chunks_exact(2)
        .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    for item in words.split(|&v| v == 0).filter(|v| !v.is_empty()) {
        let path = OsString::from_wide(item).to_string_lossy().into_owned();
        let path = path.strip_prefix(r"\??\").unwrap_or(&path);
        result.push(volume_for_path(Path::new(path))?);
    }
    Ok(result)
}

// W4 must replace these fail-closed functions with a protected handle cycle.
pub(crate) fn current_physical_block(
    _: &BlockDevice,
) -> Result<(String, BlockDevice), RestoreTargetSafetyError> {
    Err(RestoreTargetSafetyError::UnsupportedPlatform)
}
pub(crate) fn mounted_at_for_target(_: &str) -> io::Result<Vec<String>> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}
pub(crate) fn paths_name_same_device(_: &Path, _: &Path) -> bool {
    true
}
pub(crate) fn ensure_same_authorized_device(
    _: &AuthorizedRestoreTarget,
    _: &File,
) -> Result<(), RestoreDeviceError> {
    Err(RestoreDeviceError::TargetSafety(
        RestoreTargetSafetyError::UnsupportedPlatform,
    ))
}
pub(crate) fn ensure_unmounted_target(_: &str) -> Result<(), RestoreTargetSafetyError> {
    Err(RestoreTargetSafetyError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom, Write};
    use windows_sys::Win32::Storage::FileSystem::BusTypeFileBackedVirtual;
    fn snapshot() -> WindowsDiskSnapshot {
        WindowsDiskSnapshot {
            device: BlockDevice {
                path: r"\\.\PhysicalDrive7".into(),
                model: "test".into(),
                byte_len: 64 * 1024 * 1024,
                is_removable: true,
                is_read_only: false,
                boot_partitions_available: false,
            },
            pnp_instance_id: "pnp".into(),
            serial_number: "serial".into(),
            logical_sector_bytes: 512,
            physical_sector_bytes: 4096,
            bus_type: 7,
            gpt_disk_id: Some([1; 16]),
            dynamic_disk: Some(false),
            volumes: vec![],
        }
    }

    #[test]
    fn protected_disk_rejects_confirmation_before_opening_any_handle() {
        let selected = snapshot();
        assert!(matches!(
            protect_windows_disk(&selected, &[], r"\\.\PhysicalDrive99"),
            Err(WindowsPreflightError::ConfirmationMismatch)
        ));
    }

    #[test]
    fn protection_error_keeps_win32_detail_out_of_user_message() {
        let error = WindowsPreflightError::cannot_protect(
            "blokada woluminu",
            Some(2),
            io::Error::from_raw_os_error(32),
        );
        assert_eq!(
            error.to_string(),
            "Exclusive protection of the disk and its volumes could not be maintained."
        );
        let detail = format!("{error:?}");
        assert!(detail.contains("blokada woluminu"));
        assert!(detail.contains("volume_index: Some(2)"));
        assert!(detail.contains("os_error: Some(32)"));
    }

    #[test]
    fn disposable_empty_vhd_exclusive_raw_probe_when_requested() {
        let Ok(number) = std::env::var("NANDUNX_W5_EMPTY_VHD_DISK") else {
            return;
        };
        let path = format!(r"\\.\PhysicalDrive{number}");
        let selected = disks()
            .unwrap()
            .into_iter()
            .find(|disk| disk.device.path == path)
            .expect("disposable empty VHD was not enumerated");
        assert_eq!(selected.bus_type, BusTypeFileBackedVirtual);
        assert!(selected.volumes.is_empty());
        let mut protected = protect_inspected_disk(selected).expect("empty VHD RAW lock failed");
        assert!(protected.snapshot().volumes.is_empty());
        let mut table = [0_u8; 1408];
        crate::read_fixture_at(&mut protected.disk, 1024, &mut table)
            .expect("GPT table read through exclusive RAW handle failed");
    }

    #[test]
    fn locked_source_parts_deny_concurrent_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("backup.00");
        std::fs::write(&path, [0xA5; 512]).unwrap();
        let reader = crate::ImageReader::open_locked(&path).unwrap();
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        drop(reader);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_ok());
    }

    #[test]
    fn disposable_vhd_lock_and_flush_probe_when_requested() {
        let Ok(number) = std::env::var("NANDUNX_W4_VHD_DISK") else {
            return;
        };
        let path = format!(r"\\.\PhysicalDrive{number}");
        let mut selected = disks()
            .unwrap()
            .into_iter()
            .find(|disk| disk.device.path == path)
            .expect("disposable VHD was not enumerated");
        assert_eq!(selected.bus_type, BusTypeFileBackedVirtual);
        let mut names = Vec::new();
        for name in volume_names().unwrap() {
            if volume_disks(&name)
                .unwrap()
                .contains(&disk_number(&path).unwrap())
            {
                names.push(name);
            }
        }
        assert!(!names.is_empty());
        names.sort();
        selected.volumes = names;
        let mut protected =
            protect_inspected_disk(selected).expect("cannot protect disposable VHD");
        let mut changed = protected.snapshot().clone();
        changed.gpt_disk_id = Some([0x5A; 16]);
        assert!(verify_direct_handle(&protected.disk, &changed).is_err());
        let source_fixture = crate::tests::raw_nand_fixture();
        let expected = std::fs::read(source_fixture.path()).unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("synthetic-rawnand.img");
        std::fs::write(&source_path, &expected).unwrap();
        drop(source_fixture);
        let source = crate::ImageReader::open(&source_path).unwrap();
        let inspection = crate::inspect_nand_reader(&source).unwrap();
        // This offset lies inside the new, disposable VHD partition and is
        // never used against a selected physical NAND device.
        protected
            .disk
            .seek(SeekFrom::Start(4 * 1024 * 1024))
            .unwrap();
        let block = [0xA5u8; 4096];
        protected.disk.write_all(&block).unwrap();
        protected
            .flush()
            .expect("cannot flush disposable VHD handle");
        let session = crate::WindowsRestoreSession {
            source,
            inspection,
            protected,
            key: None,
            resize_plan: None,
            _keyset_file: None,
            mode: crate::OperationMode::Restore,
        };
        let result = session.execute(&mut crate::ContinueRestoreDevice).unwrap();
        assert_eq!(result.status, crate::RestoreDeviceStatus::Completed);
        let mut reopened = open_read(OsStr::new(&path)).unwrap();
        let mut readback = [0u8; 4096];
        reopened.seek(SeekFrom::Start(4 * 1024 * 1024)).unwrap();
        reopened.read_exact(&mut readback).unwrap();
        assert_eq!(readback, block);
        reopened.seek(SeekFrom::Start(0)).unwrap();
        let mut restored = vec![0u8; expected.len()];
        reopened.read_exact(&mut restored).unwrap();
        assert_eq!(restored, expected);
    }

    #[test]
    fn disposable_vhd_in_place_resize_when_requested() {
        let Ok(number) = std::env::var("NANDUNX_W4_VHD_RESIZE_DISK") else {
            return;
        };
        let path = format!(r"\\.\PhysicalDrive{number}");
        let mut selected = disks()
            .unwrap()
            .into_iter()
            .find(|disk| disk.device.path == path)
            .expect("disposable VHD was not enumerated");
        assert_eq!(selected.bus_type, BusTypeFileBackedVirtual);
        selected.volumes = volume_names()
            .unwrap()
            .into_iter()
            .filter(|name| {
                volume_disks(name)
                    .unwrap()
                    .contains(&disk_number(&path).unwrap())
            })
            .collect();
        selected.volumes.sort();
        let mut protected = protect_inspected_disk(selected).unwrap();
        let source_fixture = crate::tests::raw_nand_fixture();
        let source = crate::ImageReader::open(source_fixture.path()).unwrap();
        let inspection = crate::inspect_nand_reader(&source).unwrap();
        crate::restore_raw_nand_to_open_target(
            &source,
            &inspection,
            protected.target_mut(),
            &mut crate::ContinueRestoreDevice,
        )
        .unwrap();
        drop(source);
        let keyset = crate::tests::user_keyset_fixture();
        let key = crate::parse_bis_keyset_file(keyset.path())
            .unwrap()
            .user_key()
            .unwrap()
            .clone();
        let capacity = protected.snapshot().device.byte_len / crate::LOGICAL_SECTOR_BYTES;
        let initial =
            crate::build_fixture_resize_plan(protected.target_mut(), capacity, &key).unwrap();
        let session = crate::WindowsInPlaceSession {
            protected,
            key: key.clone(),
            _keyset_file: keyset.as_file().try_clone().unwrap(),
            initial,
        };
        let report = session.execute(&mut crate::ContinueRestoreDevice).unwrap();
        assert_eq!(report.status, crate::RestoreDeviceStatus::Completed);
        assert!(report.target_user_sectors > report.current_user_sectors);
        let mut reopened = open_read(OsStr::new(&path)).unwrap();
        let gpt = crate::parse_resize_gpt_from_fixture(&mut reopened).unwrap();
        assert_eq!(
            gpt.user_last_lba - gpt.user_first_lba + 1,
            report.target_user_sectors
        );
        let mut boot = [0u8; 512];
        crate::read_user_plain_from_fixture(
            &mut reopened,
            gpt.user_first_lba * 512,
            report.target_user_sectors * 512,
            &key,
            0,
            &mut boot,
        )
        .unwrap();
        let parsed = crate::parse_fat32_boot_sector(&boot, report.target_user_sectors).unwrap();
        assert_eq!(parsed.total_sectors as u64, report.target_user_sectors);
        let recovery = crate::inspect_recovery_reader(&mut reopened, capacity, &key).unwrap();
        assert_eq!(
            recovery.disposition,
            crate::FixtureRecoveryDisposition::Committed
        );
    }

    #[test]
    fn disposable_vhd_cancel_before_primary_commit_when_requested() {
        let Ok(number) = std::env::var("NANDUNX_W4_VHD_CANCEL_DISK") else {
            return;
        };
        let path = format!(r"\\.\PhysicalDrive{number}");
        let mut selected = disks()
            .unwrap()
            .into_iter()
            .find(|disk| disk.device.path == path)
            .unwrap();
        assert_eq!(selected.bus_type, BusTypeFileBackedVirtual);
        selected.volumes = volume_names()
            .unwrap()
            .into_iter()
            .filter(|name| {
                volume_disks(name)
                    .unwrap()
                    .contains(&disk_number(&path).unwrap())
            })
            .collect();
        selected.volumes.sort();
        let mut protected = protect_inspected_disk(selected).unwrap();
        let source_fixture = crate::tests::raw_nand_fixture();
        let source = crate::ImageReader::open(source_fixture.path()).unwrap();
        let inspection = crate::inspect_nand_reader(&source).unwrap();
        let metadata_len = inspection.primary_metadata_byte_len as usize;
        let mut old_primary = vec![0u8; metadata_len];
        protected.disk.seek(SeekFrom::Start(0)).unwrap();
        protected.disk.read_exact(&mut old_primary).unwrap();
        struct CancelAfterChunk;
        impl crate::RestoreDeviceObserver for CancelAfterChunk {
            fn on_progress(&mut self, progress: crate::RestoreDeviceProgress) -> bool {
                progress.phase != crate::RestoreDevicePhase::CopyingPayload
                    || progress.processed_bytes == 0
            }
        }
        let result = crate::restore_raw_nand_to_open_target(
            &source,
            &inspection,
            protected.target_mut(),
            &mut CancelAfterChunk,
        )
        .unwrap();
        assert!(matches!(
            result.status,
            crate::RestoreDeviceStatus::Cancelled {
                phase: crate::RestoreDevicePhase::CopyingPayload,
                processed_bytes: 16_384
            }
        ));
        drop(protected);
        let mut reopened = open_read(OsStr::new(&path)).unwrap();
        let mut still_primary = vec![0u8; metadata_len];
        reopened.read_exact(&mut still_primary).unwrap();
        assert_eq!(still_primary, old_primary);
        let expected = std::fs::read(source_fixture.path()).unwrap();
        reopened.seek(SeekFrom::Start(metadata_len as u64)).unwrap();
        let mut copied = [0u8; 16_384];
        reopened.read_exact(&mut copied).unwrap();
        assert_eq!(&copied[..], &expected[metadata_len..metadata_len + 16_384]);
    }
    #[test]
    fn rejects_changed_identity_and_4kn() {
        let selected = snapshot();
        let mut current = selected.clone();
        current.serial_number = "other".into();
        assert_eq!(
            validate_snapshot(&selected, &current, &BTreeMap::new(), &[], "system", &[]),
            Err(WindowsPreflightError::Changed)
        );
        current = selected.clone();
        current.pnp_instance_id = "replacement-in-same-slot".into();
        assert_eq!(
            validate_snapshot(&selected, &current, &BTreeMap::new(), &[], "system", &[]),
            Err(WindowsPreflightError::Changed)
        );
        let mut selected = snapshot();
        selected.logical_sector_bytes = 4096;
        assert_eq!(
            validate_snapshot(&selected, &selected, &BTreeMap::new(), &[], "system", &[]),
            Err(WindowsPreflightError::UnsupportedGeometry)
        );
        let selected = snapshot();
        let mut current = selected.clone();
        current.gpt_disk_id = Some([2; 16]);
        assert_eq!(
            validate_snapshot(&selected, &current, &BTreeMap::new(), &[], "system", &[]),
            Err(WindowsPreflightError::Changed)
        );
        let mut selected = snapshot();
        selected.serial_number.clear();
        assert_eq!(
            validate_snapshot(&selected, &selected, &BTreeMap::new(), &[], "system", &[]),
            Err(WindowsPreflightError::MissingIdentity)
        );
        let mut selected = snapshot();
        selected.dynamic_disk = Some(true);
        assert_eq!(
            validate_snapshot(&selected, &selected, &BTreeMap::new(), &[], "system", &[]),
            Err(WindowsPreflightError::UnsupportedDiskLayout)
        );
    }
    #[test]
    fn rejects_sources_system_and_ambiguous_extents() {
        let selected = snapshot();
        let mut map = BTreeMap::new();
        map.insert("system".into(), BTreeSet::from([0]));
        map.insert("source".into(), BTreeSet::from([7]));
        assert_eq!(
            validate_snapshot(
                &selected,
                &selected,
                &map,
                &["source".into()],
                "system",
                &[]
            ),
            Err(WindowsPreflightError::SourceOnTarget)
        );
        map.insert("system".into(), BTreeSet::from([7]));
        assert_eq!(
            validate_snapshot(&selected, &selected, &map, &[], "system", &[]),
            Err(WindowsPreflightError::SystemDisk)
        );
        map.insert("system".into(), BTreeSet::from([0]));
        map.insert("source".into(), BTreeSet::from([0, 7]));
        assert_eq!(
            validate_snapshot(
                &selected,
                &selected,
                &map,
                &["source".into()],
                "system",
                &[]
            ),
            Err(WindowsPreflightError::AmbiguousVolume)
        );
        map.insert("source".into(), BTreeSet::from([0]));
        map.insert("pagefile".into(), BTreeSet::from([7]));
        assert_eq!(
            validate_snapshot(
                &selected,
                &selected,
                &map,
                &[],
                "system",
                &["pagefile".into()]
            ),
            Err(WindowsPreflightError::PagefileDisk)
        );
    }

    #[test]
    fn read_only_inventory_probe_when_requested() {
        if std::env::var_os("NANDUNX_WINDOWS_INVENTORY_PROBE").is_none() {
            return;
        }
        let disks = list_windows_disks().unwrap();
        assert!(!disks.is_empty());
        assert!(disks.iter().all(|disk| disk.device.byte_len > 0));
        let program = std::env::current_exe().unwrap();
        let result = inspect_windows_disk(&disks[0], &[program]);
        assert!(
            matches!(
                result,
                Err(WindowsPreflightError::SystemDisk | WindowsPreflightError::MissingIdentity)
            ),
            "{result:?}"
        );
    }

    #[test]
    fn read_only_vhd_volume_probe_when_requested() {
        let Some(number) = std::env::var_os("NANDUNX_W3_VHD_DISK") else {
            return;
        };
        let path = format!(r"\\.\PhysicalDrive{}", number.to_string_lossy());
        let disk = list_windows_disks()
            .unwrap()
            .into_iter()
            .find(|disk| disk.device.path == path)
            .expect("owned VHD disk must be enumerated");
        assert_eq!(disk.logical_sector_bytes, 512);
        assert_eq!(disk.physical_sector_bytes, 4096);
        assert!(disk.gpt_disk_id.is_some());
        assert_eq!(disk.dynamic_disk, Some(false));
        let number = disk_number(&disk.device.path).unwrap();
        let mapped = volume_names()
            .unwrap()
            .into_iter()
            .filter(|name| volume_disks(name).is_ok_and(|numbers| numbers.contains(&number)))
            .count();
        assert!(
            mapped > 0,
            "owned VHD partition must appear in volume extents"
        );
        let result = inspect_windows_disk(&disk, &[]);
        assert!(
            result.is_ok() || matches!(result, Err(WindowsPreflightError::MissingIdentity)),
            "{result:?}"
        );
    }
}
