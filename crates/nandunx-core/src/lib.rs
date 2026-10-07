//! Pure, UI-independent planning primitives for NANDuNX.
//!
//! Disk access, parsing of real NAND structures and cryptography intentionally
//! remain unimplemented until they are covered by synthetic fixtures.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use aes::{
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit},
    Aes128,
};
use crc32fast::Hasher;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::cell::RefCell;

mod device;
#[cfg(all(test, target_os = "linux"))]
use device::linux::{
    is_physical_device_name, mountinfo_path_from_environment, parse_mountinfo_line,
    source_mentions_target, sysfs_parent_block_name, unescape_mountinfo_path,
};
#[cfg(target_os = "windows")]
pub use device::windows::{
    inspect_windows_disk, list_windows_disks, protect_windows_disk, WindowsDiskSnapshot,
    WindowsPreflightError, WindowsProtectedDisk,
};

#[cfg(target_os = "windows")]
pub struct WindowsRestorePreflight {
    pub disk: WindowsDiskSnapshot,
    pub backup: PreflightReport,
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub enum WindowsRestorePreflightError {
    Device(WindowsPreflightError),
    Source(PreflightError),
}

/// A read-only Windows restore preflight. Every split part and supplied file
/// is mapped to physical disk extents before the backup parser is run.
#[cfg(target_os = "windows")]
pub fn preflight_windows_nand_restore(
    selected: &WindowsDiskSnapshot,
    plan: OperationPlan,
    backup_path: impl AsRef<Path>,
    keyset_path: Option<&Path>,
    boot0_path: Option<&Path>,
    boot1_path: Option<&Path>,
) -> Result<WindowsRestorePreflight, WindowsRestorePreflightError> {
    if selected.device != plan.target {
        return Err(WindowsRestorePreflightError::Device(
            WindowsPreflightError::Changed,
        ));
    }
    let backup_path = backup_path.as_ref();
    let image = ImageReader::open(backup_path)
        .map_err(|error| WindowsRestorePreflightError::Source(PreflightError::Inspection(error)))?;
    let mut paths = image
        .parts
        .into_iter()
        .map(|part| part.path)
        .collect::<Vec<_>>();
    paths.extend(keyset_path.into_iter().map(Path::to_path_buf));
    paths.extend(boot0_path.into_iter().map(Path::to_path_buf));
    paths.extend(boot1_path.into_iter().map(Path::to_path_buf));
    let disk =
        inspect_windows_disk(selected, &paths).map_err(WindowsRestorePreflightError::Device)?;
    let backup = preflight_nand_restore(plan, backup_path, keyset_path, boot0_path, boot1_path)
        .map_err(WindowsRestorePreflightError::Source)?;
    Ok(WindowsRestorePreflight { disk, backup })
}

/// Read-only USER validation on a Windows physical disk. This does not issue
/// a write token; W4 must recheck the snapshot under volume protection.
#[cfg(target_os = "windows")]
pub fn preflight_windows_in_place_user_expansion(
    selected: &WindowsDiskSnapshot,
    plan: InPlaceExpansionPlan,
    key: &BisKey,
    source_paths: &[PathBuf],
) -> Result<InPlaceExpansionPreflight, InPlaceExpansionError> {
    if selected.device != plan.target {
        return Err(InPlaceExpansionError::TargetChanged);
    }
    let disk = inspect_windows_disk(selected, source_paths)
        .map_err(InPlaceExpansionError::WindowsDevice)?;
    let mut device =
        File::open(&disk.device.path).map_err(|_| InPlaceExpansionError::TargetUnavailable)?;
    let resize = build_fixture_resize_plan(
        &mut device,
        disk.device.byte_len / LOGICAL_SECTOR_BYTES,
        key,
    )
    .map_err(InPlaceExpansionError::PlanningDetail)?;
    Ok(in_place_preflight_from_resize_plan(plan, &resize))
}

/// Windows writer preparation holds every split source part and the keyset
/// open, then obtains a protected physical handle through the public API.
/// Callers must run this in the desktop process elevated by UAC.
#[cfg(target_os = "windows")]
pub struct WindowsRestoreSession {
    source: ImageReader,
    inspection: RawNandInspection,
    protected: WindowsProtectedDisk,
    key: Option<BisKey>,
    resize_plan: Option<FixtureResizePlan>,
    _keyset_file: Option<File>,
    mode: OperationMode,
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub enum WindowsOperationError {
    Device(WindowsPreflightError),
    Source(InspectionError),
    Keyset(KeysetFileError),
    User(UserFilesystemError),
    Planning,
    PlanningDetail(RestoreResizeFixtureError),
    Write(RestoreDeviceError),
    Recovery(FixtureRecoveryError),
}

#[cfg(target_os = "windows")]
impl fmt::Display for WindowsOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(e) => e.fmt(f),
            Self::Source(e) => e.fmt(f),
            Self::Keyset(e) => e.fmt(f),
            Self::User(e) => e.fmt(f),
            Self::Planning => f.write_str("Nie można bezpiecznie zaplanować operacji Windows."),
            Self::PlanningDetail(_) => {
                f.write_str("Nie można bezpiecznie zaplanować operacji Windows.")
            }
            Self::Write(e) => e.fmt(f),
            Self::Recovery(_) => f.write_str("Nie można odczytać raportu recovery z dysku."),
        }
    }
}

#[cfg(target_os = "windows")]
pub fn authorize_windows_restore(
    selected: &WindowsDiskSnapshot,
    mode: OperationMode,
    backup_path: &Path,
    keyset_path: Option<&Path>,
    other_source_paths: &[PathBuf],
    typed_confirmation: &str,
) -> Result<WindowsRestoreSession, WindowsOperationError> {
    if !matches!(
        mode,
        OperationMode::Restore | OperationMode::RestoreAndExpandUser
    ) {
        return Err(WindowsOperationError::Planning);
    }
    let source = ImageReader::open_locked(backup_path).map_err(WindowsOperationError::Source)?;
    let inspection = inspect_nand_reader(&source).map_err(WindowsOperationError::Source)?;
    if inspection.image_byte_len > selected.device.byte_len {
        return Err(WindowsOperationError::Planning);
    }
    let mut paths = source
        .parts
        .iter()
        .map(|part| part.path.clone())
        .collect::<Vec<_>>();
    paths.extend_from_slice(other_source_paths);
    let (keyset_file, key) = if mode == OperationMode::RestoreAndExpandUser {
        let path = keyset_path.ok_or(WindowsOperationError::Planning)?;
        paths.push(path.to_path_buf());
        let (file, keys) =
            parse_bis_keyset_held_file(path).map_err(WindowsOperationError::Keyset)?;
        let key = keys
            .user_key()
            .cloned()
            .ok_or(WindowsOperationError::Planning)?;
        let (fat32, fat) = read_user_fat32_reader(
            &source,
            inspection.raw_nand_offset_bytes,
            &inspection.user_partition,
            &key,
        )
        .map_err(WindowsOperationError::User)?;
        let current_sectors =
            inspection.user_partition.last_lba - inspection.user_partition.first_lba + 1;
        let expansion = plan_user_expansion(
            selected.device.byte_len / LOGICAL_SECTOR_BYTES,
            inspection.user_partition.first_lba,
            current_sectors,
        )
        .map_err(|_| WindowsOperationError::Planning)?;
        let fat_expansion = plan_fat32_expansion(&fat32.boot_sector, expansion.target_user_sectors)
            .map_err(|_| WindowsOperationError::Planning)?;
        plan_fat32_cluster_relocations(&fat32.boot_sector, &fat, &fat_expansion)
            .map_err(WindowsOperationError::User)?;
        build_expanded_fat(&fat, &fat_expansion).map_err(WindowsOperationError::User)?;
        (Some(file), Some(key))
    } else {
        (None, None)
    };
    let resize_plan = key
        .as_ref()
        .map(|key| {
            build_resize_plan_from_source(
                &source,
                inspection.raw_nand_offset_bytes,
                selected.device.byte_len / LOGICAL_SECTOR_BYTES,
                key,
            )
        })
        .transpose()
        .map_err(WindowsOperationError::PlanningDetail)?;
    let protected = protect_windows_disk(selected, &paths, typed_confirmation)
        .map_err(WindowsOperationError::Device)?;
    Ok(WindowsRestoreSession {
        source,
        inspection,
        protected,
        key,
        resize_plan,
        _keyset_file: keyset_file,
        mode,
    })
}

#[cfg(target_os = "windows")]
impl WindowsRestoreSession {
    pub fn execute(
        mut self,
        observer: &mut impl RestoreDeviceObserver,
    ) -> Result<RestoreDeviceReport, WindowsOperationError> {
        let copied = restore_raw_nand_to_open_target(
            &self.source,
            &self.inspection,
            self.protected.target_mut(),
            observer,
        )
        .map_err(WindowsOperationError::Write)?;
        if !matches!(copied.status, RestoreDeviceStatus::Completed)
            || self.mode == OperationMode::Restore
        {
            return Ok(copied);
        }
        let key = self.key.as_ref().ok_or(WindowsOperationError::Planning)?;
        // The complete plan was built from the locked source before any
        // target write. Byte-for-byte payload and primary GPT verification
        // above establish that this plan describes the restored target.
        let resize = self
            .resize_plan
            .take()
            .ok_or(WindowsOperationError::Planning)?;
        run_device_resize_phases(
            self.protected.target_mut(),
            &resize,
            key,
            self.inspection.image_byte_len,
            observer,
        )
        .map_err(WindowsOperationError::Write)
    }
}

#[cfg(target_os = "windows")]
pub struct WindowsInPlaceSession {
    protected: WindowsProtectedDisk,
    key: BisKey,
    _keyset_file: File,
    initial: FixtureResizePlan,
}

/// Read-only validation and protection for in-place `USER` expansion.
#[cfg(target_os = "windows")]
pub fn authorize_windows_in_place(
    selected: &WindowsDiskSnapshot,
    keyset_path: &Path,
    other_source_paths: &[PathBuf],
    typed_confirmation: &str,
) -> Result<WindowsInPlaceSession, WindowsOperationError> {
    let (keyset_file, keys) =
        parse_bis_keyset_held_file(keyset_path).map_err(WindowsOperationError::Keyset)?;
    let key = keys
        .user_key()
        .cloned()
        .ok_or(WindowsOperationError::Planning)?;
    let mut sources = other_source_paths.to_vec();
    sources.push(keyset_path.to_path_buf());
    inspect_windows_disk(selected, &sources).map_err(WindowsOperationError::Device)?;
    let mut read_only =
        File::open(&selected.device.path).map_err(|_| WindowsOperationError::Planning)?;
    let capacity = selected.device.byte_len / LOGICAL_SECTOR_BYTES;
    let initial = build_fixture_resize_plan(&mut read_only, capacity, &key)
        .map_err(|_| WindowsOperationError::Planning)?;
    drop(read_only);
    let protected = protect_windows_disk(selected, &sources, typed_confirmation)
        .map_err(WindowsOperationError::Device)?;
    Ok(WindowsInPlaceSession {
        protected,
        key,
        _keyset_file: keyset_file,
        initial,
    })
}

#[cfg(target_os = "windows")]
impl WindowsInPlaceSession {
    pub fn execute(
        mut self,
        observer: &mut impl RestoreDeviceObserver,
    ) -> Result<InPlaceExpansionReport, WindowsOperationError> {
        let capacity = self.protected.snapshot().device.byte_len / LOGICAL_SECTOR_BYTES;
        let current = build_fixture_resize_plan(self.protected.target_mut(), capacity, &self.key)
            .map_err(|_| WindowsOperationError::Planning)?;
        if current.expansion != self.initial.expansion
            || current.gpt.user_first_lba != self.initial.gpt.user_first_lba
            || current.fat32 != self.initial.fat32
        {
            return Err(WindowsOperationError::Planning);
        }
        let result = run_device_resize_phases(
            self.protected.target_mut(),
            &current,
            &self.key,
            0,
            observer,
        )
        .map_err(WindowsOperationError::Write)?;
        Ok(InPlaceExpansionReport {
            status: result.status,
            current_user_sectors: current.expansion.current_user_sectors,
            target_user_sectors: current.expansion.target_user_sectors,
        })
    }
}

/// Independent, read-only recovery view for the same physical disk after a
/// failed Windows write or a desktop restart. It never repairs metadata.
#[cfg(target_os = "windows")]
pub fn inspect_windows_recovery(
    selected: &WindowsDiskSnapshot,
    keyset_path: &Path,
) -> Result<FixtureRecoveryReport, WindowsOperationError> {
    let (_guard, keyset) =
        parse_bis_keyset_held_file(keyset_path).map_err(WindowsOperationError::Keyset)?;
    let key = keyset.user_key().ok_or(WindowsOperationError::Planning)?;
    let current = list_windows_disks()
        .map_err(|_| WindowsOperationError::Device(WindowsPreflightError::Io))?
        .into_iter()
        .find(|disk| disk.device.path == selected.device.path)
        .ok_or(WindowsOperationError::Device(
            WindowsPreflightError::Changed,
        ))?;
    if current.pnp_instance_id != selected.pnp_instance_id
        || current.serial_number != selected.serial_number
        || current.device.byte_len != selected.device.byte_len
        || current.logical_sector_bytes != 512
    {
        return Err(WindowsOperationError::Device(
            WindowsPreflightError::Changed,
        ));
    }
    let mut file = File::open(&current.device.path)
        .map_err(|_| WindowsOperationError::Recovery(FixtureRecoveryError::TargetUnavailable))?;
    inspect_recovery_reader(
        &mut file,
        current.device.byte_len / LOGICAL_SECTOR_BYTES,
        key,
    )
    .map_err(WindowsOperationError::Recovery)
}
use device::{
    current_physical_block, ensure_unmounted_target, mounted_at_for_target, paths_name_same_device,
};

pub const LOGICAL_SECTOR_BYTES: u64 = 512;
pub const USER_CLUSTER_SECTORS: u64 = 32;
pub const BACKUP_GPT_SECTORS: u64 = 33;
pub const MAX_ARTIFACT_BYTES: u64 = 1 << 40; // 1 TiB
pub const MAX_KEYSET_BYTES: u64 = 1024 * 1024;
/// The restore+resize model intentionally operates on fixtures in memory. A
/// bounded size prevents a test helper from becoming an accidental bulk-image
/// allocator; real-media writes belong to the later transactional layer.
pub const MAX_SYNTHETIC_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
/// Every Switch eMMC boot area is exactly 4 MiB. A FULL NAND image accepted by
/// NANDuNX is the explicit concatenation `BOOT0 || BOOT1 || RAWNAND`.
pub const BOOT_PARTITION_BYTES: u64 = 4 * 1024 * 1024;
pub const FULL_NAND_BOOT_AREA_BYTES: u64 = BOOT_PARTITION_BYTES * 2;
const GPT_HEADER_LBA: u64 = 1;
const GPT_MIN_HEADER_SIZE: usize = 92;
const GPT_ENTRY_MIN_SIZE: usize = 128;
const MAX_GPT_TABLE_BYTES: usize = 16 * 1024 * 1024;
const RESTORE_FIXTURE_CHUNK_BYTES: usize = 16 * 1024;

/// Switch NAND encryption is applied to fixed 16 KiB data units, rather than
/// to individual 512-byte logical sectors.
pub const BIS_DATA_UNIT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Backup,
    Keyset,
    Boot0,
    Boot1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactReceipt {
    pub id: String,
    pub kind: ArtifactKind,
    pub byte_len: u64,
}

/// One AES-128-XTS key pair from a user-provided keyset. Its fields are not
/// exposed or printable so UI adapters cannot accidentally log key material.
#[derive(Clone, PartialEq, Eq)]
pub struct BisKey {
    crypt: [u8; 16],
    tweak: [u8; 16],
}

/// Parsed BIS keys. Only the `USER`/`SYSTEM` pair (BIS key 2) is required for
/// the first resizing workflow; the remaining pairs are preserved for later
/// read-only validation of the other encrypted partitions.
pub struct BisKeyset {
    keys: [Option<BisKey>; 4],
}

impl BisKeyset {
    pub fn user_key(&self) -> Option<&BisKey> {
        self.keys[2].as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysetError {
    InvalidBisKey,
    DuplicateBisKeyPart,
    IncompleteBisKey,
    MissingUserKey,
}

impl fmt::Display for KeysetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBisKey => write!(formatter, "Keyset zawiera nieprawidłowy klucz BIS."),
            Self::DuplicateBisKeyPart => write!(formatter, "Keyset powtarza część klucza BIS."),
            Self::IncompleteBisKey => write!(formatter, "Keyset zawiera niepełny klucz BIS."),
            Self::MissingUserKey => write!(
                formatter,
                "Keyset nie zawiera pełnego BIS Key 2 wymaganego dla USER."
            ),
        }
    }
}

/// Parses the two common text representations produced by user-facing key
/// tools: `bis_key_02 = <64 hex chars>` and separate
/// `BIS Key 2 (crypt|tweak): <32 hex chars>` lines. The returned value never
/// retains the original text and error messages never contain key material.
pub fn parse_bis_keyset(input: &str) -> Result<BisKeyset, KeysetError> {
    let mut halves: [[Option<[u8; 16]>; 2]; 4] =
        [[None, None], [None, None], [None, None], [None, None]];

    for line in input.lines() {
        let line = line.trim();
        let lowercase = line.to_ascii_lowercase();
        let parsed = if let Some((label, value)) = lowercase.split_once('=') {
            parse_combined_bis_key(label.trim(), value.trim())?
        } else if let Some((label, value)) = lowercase.split_once(':') {
            parse_separate_bis_key(label.trim(), value.trim())?
        } else {
            None
        };

        let Some((index, crypt, tweak)) = parsed else {
            continue;
        };
        if let Some(crypt) = crypt {
            if halves[index][0].replace(crypt).is_some() {
                return Err(KeysetError::DuplicateBisKeyPart);
            }
        }
        if let Some(tweak) = tweak {
            if halves[index][1].replace(tweak).is_some() {
                return Err(KeysetError::DuplicateBisKeyPart);
            }
        }
    }

    let mut keys = [None, None, None, None];
    for (index, [crypt, tweak]) in halves.into_iter().enumerate() {
        match (crypt, tweak) {
            (None, None) => {}
            (Some(crypt), Some(tweak)) => keys[index] = Some(BisKey { crypt, tweak }),
            _ => return Err(KeysetError::IncompleteBisKey),
        }
    }
    if keys[2].is_none() {
        return Err(KeysetError::MissingUserKey);
    }
    Ok(BisKeyset { keys })
}

/// Reads at most one MiB of a local keyset and parses it without logging or
/// persisting its contents.
pub fn parse_bis_keyset_file(path: impl AsRef<Path>) -> Result<BisKeyset, KeysetFileError> {
    let file = File::open(path).map_err(|_| KeysetFileError::Unavailable)?;
    parse_bis_keyset_open_handle(file).map(|(_, keyset)| keyset)
}

#[cfg(target_os = "windows")]
fn parse_bis_keyset_held_file(path: &Path) -> Result<(File, BisKeyset), KeysetFileError> {
    let file = open_source_file_locked(path).map_err(|_| KeysetFileError::Unavailable)?;
    parse_bis_keyset_open_handle(file)
}

fn parse_bis_keyset_open_handle(mut file: File) -> Result<(File, BisKeyset), KeysetFileError> {
    let metadata = file.metadata().map_err(|_| KeysetFileError::Unavailable)?;
    if !metadata.is_file() {
        return Err(KeysetFileError::Unavailable);
    }
    if metadata.len() > MAX_KEYSET_BYTES {
        return Err(KeysetFileError::TooLarge);
    }
    let mut input = String::new();
    Read::by_ref(&mut file)
        .take(MAX_KEYSET_BYTES + 1)
        .read_to_string(&mut input)
        .map_err(|_| KeysetFileError::Unavailable)?;
    if input.len() as u64 > MAX_KEYSET_BYTES {
        return Err(KeysetFileError::TooLarge);
    }
    let keyset = parse_bis_keyset(&input).map_err(KeysetFileError::InvalidFormat)?;
    Ok((file, keyset))
}

fn parse_combined_bis_key(
    label: &str,
    value: &str,
) -> Result<Option<(usize, Option<[u8; 16]>, Option<[u8; 16]>)>, KeysetError> {
    let Some(index) = label.strip_prefix("bis_key_0") else {
        return Ok(None);
    };
    let index = index
        .parse::<usize>()
        .map_err(|_| KeysetError::InvalidBisKey)?;
    if index > 3 {
        return Ok(None);
    }
    let bytes = decode_hex(value, 32)?;
    let crypt: [u8; 16] = bytes[..16].try_into().expect("fixed key length");
    let tweak: [u8; 16] = bytes[16..].try_into().expect("fixed key length");
    Ok(Some((index, Some(crypt), Some(tweak))))
}

fn parse_separate_bis_key(
    label: &str,
    value: &str,
) -> Result<Option<(usize, Option<[u8; 16]>, Option<[u8; 16]>)>, KeysetError> {
    let Some(rest) = label.strip_prefix("bis key ") else {
        return Ok(None);
    };
    let (index, part) = rest.split_once(' ').ok_or(KeysetError::InvalidBisKey)?;
    let index = index
        .parse::<usize>()
        .map_err(|_| KeysetError::InvalidBisKey)?;
    if index > 3 {
        return Ok(None);
    }
    let value: [u8; 16] = decode_hex(value, 16)?.try_into().expect("fixed key length");
    match part {
        "(crypt)" => Ok(Some((index, Some(value), None))),
        "(tweak)" => Ok(Some((index, None, Some(value)))),
        _ => Err(KeysetError::InvalidBisKey),
    }
}

fn decode_hex(value: &str, expected_len: usize) -> Result<Vec<u8>, KeysetError> {
    if value.len() != expected_len * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(KeysetError::InvalidBisKey);
    }
    (0..value.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&value[offset..offset + 2], 16)
                .map_err(|_| KeysetError::InvalidBisKey)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    InvalidDataUnitSize,
}

/// Independent AES-128-XTS implementation of the Switch NAND data-unit
/// profile. `unit_index` is relative to the encrypted partition and every
/// input is exactly one 16 KiB data unit.
pub struct SwitchAesXts128 {
    crypt: Aes128,
    tweak: Aes128,
}

impl SwitchAesXts128 {
    pub fn new(key: &BisKey) -> Self {
        Self {
            crypt: Aes128::new_from_slice(&key.crypt).expect("AES-128 key length"),
            tweak: Aes128::new_from_slice(&key.tweak).expect("AES-128 key length"),
        }
    }

    pub fn encrypt_unit(&self, unit_index: u64, data: &mut [u8]) -> Result<(), CryptoError> {
        self.crypt_unit(unit_index, data, true)
    }

    pub fn decrypt_unit(&self, unit_index: u64, data: &mut [u8]) -> Result<(), CryptoError> {
        self.crypt_unit(unit_index, data, false)
    }

    fn crypt_unit(
        &self,
        unit_index: u64,
        data: &mut [u8],
        encrypt: bool,
    ) -> Result<(), CryptoError> {
        if data.len() != BIS_DATA_UNIT_BYTES || data.len() % 16 != 0 {
            return Err(CryptoError::InvalidDataUnitSize);
        }

        let mut tweak = [0_u8; 16];
        tweak[8..].copy_from_slice(&unit_index.to_be_bytes());
        let mut tweak_block = aes::Block::from(tweak);
        self.tweak.encrypt_block(&mut tweak_block);
        tweak.copy_from_slice(&tweak_block);

        for chunk in data.chunks_exact_mut(16) {
            for (byte, mask) in chunk.iter_mut().zip(tweak) {
                *byte ^= mask;
            }
            let mut block = aes::Block::clone_from_slice(chunk);
            if encrypt {
                self.crypt.encrypt_block(&mut block);
            } else {
                self.crypt.decrypt_block(&mut block);
            }
            for ((destination, encrypted), mask) in chunk.iter_mut().zip(block.iter()).zip(tweak) {
                *destination = mask ^ *encrypted;
            }
            multiply_tweak_by_alpha(&mut tweak);
        }
        Ok(())
    }
}

fn multiply_tweak_by_alpha(tweak: &mut [u8; 16]) {
    let carry = tweak[15] & 0x80 != 0;
    for index in (1..16).rev() {
        tweak[index] = (tweak[index] << 1) | (tweak[index - 1] >> 7);
    }
    tweak[0] <<= 1;
    if carry {
        tweak[0] ^= 0x87;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationMode {
    Restore,
    RestoreAndExpandUser,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BlockDevice {
    pub path: String,
    pub model: String,
    pub byte_len: u64,
    pub is_removable: bool,
    pub is_read_only: bool,
    pub boot_partitions_available: bool,
}

/// A target that has passed the platform-specific barriers required before a
/// future write engine may open it for writing. The held descriptor is opened
/// read-only and exclusively locked; this type deliberately offers no writing
/// API. Dropping it releases the advisory lock.
pub struct AuthorizedRestoreTarget {
    file: File,
    pub canonical_path: String,
    pub byte_len: u64,
}

impl Drop for AuthorizedRestoreTarget {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoreTargetPreflight {
    pub canonical_path: String,
    pub byte_len: u64,
    pub required_confirmation: String,
    pub mounted_at: Vec<String>,
}

#[derive(Debug)]
pub enum RestoreTargetSafetyError {
    UnsupportedPlatform,
    TargetUnavailable,
    TargetNotPhysicalDevice,
    TargetReadOnly,
    TargetChanged,
    TargetMounted,
    SourceMatchesTarget,
    ConfirmationMismatch,
    CannotLock,
}

impl fmt::Display for RestoreTargetSafetyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => write!(
                formatter,
                "Zapis na urządzeniu nie jest obsługiwany na tej platformie."
            ),
            Self::TargetUnavailable => write!(formatter, "Wybrane urządzenie nie jest dostępne."),
            Self::TargetNotPhysicalDevice => write!(
                formatter,
                "Celem musi być główne fizyczne urządzenie blokowe."
            ),
            Self::TargetReadOnly => write!(formatter, "Wybrane urządzenie jest tylko do odczytu."),
            Self::TargetChanged => write!(
                formatter,
                "Parametry wybranego urządzenia zmieniły się od czasu preflightu."
            ),
            Self::TargetMounted => write!(
                formatter,
                "Wybrane urządzenie lub jego partycja jest zamontowane."
            ),
            Self::SourceMatchesTarget => write!(
                formatter,
                "Źródło backupu i cel wskazują to samo urządzenie."
            ),
            Self::ConfirmationMismatch => write!(
                formatter,
                "Potwierdzenie musi być dokładną, pełną ścieżką urządzenia."
            ),
            Self::CannotLock => {
                write!(formatter, "Nie można uzyskać wyłącznej blokady urządzenia.")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationPlan {
    pub mode: OperationMode,
    pub target: BlockDevice,
    pub backup: ArtifactReceipt,
    pub keyset: Option<ArtifactReceipt>,
    pub boot0: Option<ArtifactReceipt>,
    pub boot1: Option<ArtifactReceipt>,
    pub requires_read_only_preflight: bool,
    pub write_operations_enabled: bool,
}

/// A non-destructive plan for expanding the `USER` partition already present
/// on one physical device. Unlike `OperationPlan`, it intentionally contains
/// no backup source: the selected device is both the read-only preflight
/// source and, only after explicit authorisation, the write target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InPlaceExpansionPlan {
    pub target: BlockDevice,
    pub requires_read_only_preflight: bool,
    pub write_operations_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InPlaceExpansionPreflight {
    pub plan: InPlaceExpansionPlan,
    pub user_partition: GptPartition,
    pub expanded_user: UserExpansionPlan,
    pub user_fat32: Fat32Inspection,
    pub fat32_expansion: Fat32ExpansionPlan,
    pub relocation_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct InPlaceExpansionReport {
    pub status: RestoreDeviceStatus,
    pub current_user_sectors: u64,
    pub target_user_sectors: u64,
}

#[derive(Debug)]
pub enum InPlaceExpansionError {
    TargetSafety(RestoreTargetSafetyError),
    #[cfg(target_os = "windows")]
    WindowsDevice(WindowsPreflightError),
    Planning,
    PlanningDetail(RestoreResizeFixtureError),
    TargetChanged,
    TargetUnavailable,
    WriteFailed,
    VerificationFailed,
}

impl fmt::Display for InPlaceExpansionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetSafety(error) => error.fmt(formatter),
            #[cfg(target_os = "windows")]
            Self::WindowsDevice(error) => error.fmt(formatter),
            Self::Planning => write!(
                formatter,
                "Nie można bezpiecznie zaplanować rozszerzenia USER na tym urządzeniu."
            ),
            Self::PlanningDetail(_) => write!(
                formatter,
                "Nie można bezpiecznie zaplanować rozszerzenia USER na tym urządzeniu."
            ),
            Self::TargetChanged => {
                write!(formatter, "Urządzenie zmieniło się od czasu preflightu.")
            }
            Self::TargetUnavailable => write!(
                formatter,
                "Nie można otworzyć urządzenia do rozszerzenia USER."
            ),
            Self::WriteFailed => write!(formatter, "Zapis fazy rozszerzenia USER nie powiódł się."),
            Self::VerificationFailed => write!(
                formatter,
                "Weryfikacja rozszerzonego USER nie powiodła się."
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationPlanError {
    EmptyArtifact,
    ArtifactTooLarge,
    KeysetTooLarge,
    ArtifactKindsDoNotMatch,
    KeysetRequired,
    IncompleteBootBackup,
    TargetReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GptPartition {
    pub name: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RawNandInspection {
    pub image_byte_len: u64,
    pub container_byte_len: u64,
    pub source_part_count: usize,
    pub format: NandImageFormat,
    pub raw_nand_offset_bytes: u64,
    /// Bytes at the start of RAWNAND containing the protective MBR and the
    /// primary GPT. A device restore defers this range until its final commit.
    pub primary_metadata_byte_len: u64,
    pub boot0_source: BootComponentSource,
    pub boot1_source: BootComponentSource,
    pub backup_gpt_lba: u64,
    pub partitions: Vec<GptPartition>,
    pub user_partition: GptPartition,
}

/// The only container layouts accepted at this stage. Recognition tests the
/// GPT at the exact, documented location; NANDuNX never searches an image for a
/// plausible partition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NandImageFormat {
    RawNand,
    FullNand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootComponentSource {
    Embedded,
    Supplied,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreflightReport {
    pub plan: OperationPlan,
    pub source: RawNandInspection,
    pub expanded_user: Option<UserExpansionPlan>,
    pub keyset_validated_against_user_fat32: bool,
    pub user_fat32: Option<Fat32Inspection>,
    pub fat32_expansion: Option<Fat32ExpansionPlan>,
}

#[derive(Debug)]
pub enum PreflightError {
    Inspection(InspectionError),
    Keyset(KeysetFileError),
    UserFilesystem(UserFilesystemError),
    Fat32Plan(Fat32PlanError),
    TargetTooSmall,
    CannotExpandUser(PlanError),
    BootBackup(BootBackupError),
}

impl fmt::Display for PreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection(error) => error.fmt(formatter),
            Self::Keyset(error) => error.fmt(formatter),
            Self::UserFilesystem(error) => error.fmt(formatter),
            Self::Fat32Plan(_) => write!(
                formatter,
                "Nie można bezpiecznie zaplanować rozszerzenia FAT32."
            ),
            Self::TargetTooSmall => write!(
                formatter,
                "Docelowy nośnik jest mniejszy niż obraz backupu."
            ),
            Self::CannotExpandUser(PlanError::TargetEndsBeforeUser) => {
                write!(
                    formatter,
                    "Docelowy nośnik nie pozostawia miejsca dla USER i zapasowego GPT."
                )
            }
            Self::CannotExpandUser(PlanError::NoCapacityGain) => {
                write!(
                    formatter,
                    "Docelowy nośnik nie daje dodatkowej przestrzeni dla USER."
                )
            }
            Self::BootBackup(error) => error.fmt(formatter),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootBackupError {
    IncompletePair,
    UnexpectedForFullNand,
    Unavailable,
    IncorrectSize,
}

impl fmt::Display for BootBackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompletePair => {
                write!(formatter, "BOOT0 i BOOT1 muszą być podane jako komplet.")
            }
            Self::UnexpectedForFullNand => write!(
                formatter,
                "Obraz FULL NAND zawiera już BOOT0 i BOOT1; nie dodawaj ich osobno."
            ),
            Self::Unavailable => write!(
                formatter,
                "Backup BOOT0 lub BOOT1 nie jest dostępny w tej sesji."
            ),
            Self::IncorrectSize => {
                write!(formatter, "BOOT0 i BOOT1 muszą mieć dokładnie po 4 MiB.")
            }
        }
    }
}

/// Performs read-only source validation for an explicitly recognised RAWNAND
/// or FULL NAND image. Resize additionally validates a safe-sized BIS keyset
/// against the encrypted `USER` FAT32 boot sector. It never opens the target
/// for writing.
pub fn preflight_nand_restore(
    plan: OperationPlan,
    backup_path: impl AsRef<Path>,
    keyset_path: Option<&Path>,
    boot0_path: Option<&Path>,
    boot1_path: Option<&Path>,
) -> Result<PreflightReport, PreflightError> {
    let backup_path = backup_path.as_ref();
    let user_key = if plan.mode == OperationMode::RestoreAndExpandUser {
        let keyset_path =
            keyset_path.ok_or(PreflightError::Keyset(KeysetFileError::Unavailable))?;
        Some(
            parse_bis_keyset_file(keyset_path)
                .map_err(PreflightError::Keyset)?
                .user_key()
                .cloned()
                .expect("BIS Key 2 is required by the parser"),
        )
    } else {
        None
    };
    if plan.boot0.is_some() != boot0_path.is_some() || plan.boot1.is_some() != boot1_path.is_some()
    {
        return Err(PreflightError::BootBackup(BootBackupError::Unavailable));
    }
    let mut source = inspect_nand_image(backup_path).map_err(PreflightError::Inspection)?;
    let (boot0_source, boot1_source) = validate_boot_backups(source.format, boot0_path, boot1_path)
        .map_err(PreflightError::BootBackup)?;
    source.boot0_source = boot0_source;
    source.boot1_source = boot1_source;
    if plan.target.byte_len < source.image_byte_len {
        return Err(PreflightError::TargetTooSmall);
    }

    let expanded_user = if plan.mode == OperationMode::RestoreAndExpandUser {
        let current_user_sectors =
            source.user_partition.last_lba - source.user_partition.first_lba + 1;
        Some(
            plan_user_expansion(
                plan.target.byte_len / LOGICAL_SECTOR_BYTES,
                source.user_partition.first_lba,
                current_user_sectors,
            )
            .map_err(PreflightError::CannotExpandUser)?,
        )
    } else {
        None
    };
    let user_fat32 = if let Some(user_key) = user_key.as_ref() {
        Some(
            validate_user_fat32(
                backup_path,
                source.raw_nand_offset_bytes,
                &source.user_partition,
                user_key,
            )
            .map_err(PreflightError::UserFilesystem)?,
        )
    } else {
        None
    };
    let keyset_validated_against_user_fat32 = user_fat32.is_some();
    let fat32_expansion = match (&user_fat32, &expanded_user) {
        (Some(fat32), Some(expanded_user)) => Some(
            plan_fat32_expansion(&fat32.boot_sector, expanded_user.target_user_sectors)
                .map_err(PreflightError::Fat32Plan)?,
        ),
        _ => None,
    };

    Ok(PreflightReport {
        plan,
        source,
        expanded_user,
        keyset_validated_against_user_fat32,
        user_fat32,
        fat32_expansion,
    })
}

/// Compatibility wrapper for callers that intentionally provide RAWNAND only.
pub fn preflight_raw_nand_restore(
    plan: OperationPlan,
    backup_path: impl AsRef<Path>,
    keyset_path: Option<&Path>,
) -> Result<PreflightReport, PreflightError> {
    preflight_nand_restore(plan, backup_path, keyset_path, None, None)
}

/// Phases exposed by the file-fixture restore engine. They are deliberately
/// independent from UI wording so both adapters can return structured
/// progress without interpreting I/O errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreFixturePhase {
    Copying,
    Verifying,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RestoreFixtureProgress {
    pub phase: RestoreFixturePhase,
    pub processed_bytes: u64,
    pub total_bytes: u64,
}

/// Called at safe chunk boundaries. Returning `false` requests cancellation;
/// the engine never begins a new chunk after that point.
pub trait RestoreFixtureObserver {
    fn on_progress(&mut self, progress: RestoreFixtureProgress) -> bool;
}

/// An observer for callers that only need a final verification outcome.
pub struct ContinueRestoreFixture;

impl RestoreFixtureObserver for ContinueRestoreFixture {
    fn on_progress(&mut self, _: RestoreFixtureProgress) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreFixtureStatus {
    Completed,
    Cancelled {
        phase: RestoreFixturePhase,
        processed_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoreFixtureReport {
    pub status: RestoreFixtureStatus,
    pub raw_nand_bytes: u64,
    pub verified_bytes: u64,
}

#[derive(Debug)]
pub enum RestoreFixtureError {
    Inspection(InspectionError),
    SourceUnavailable,
    TargetUnavailable,
    SourceMatchesTarget,
    TargetIsNotRegularFile,
    TargetTooSmall,
    CopyFailed,
    VerificationReadFailed,
    VerificationMismatch,
}

impl fmt::Display for RestoreFixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection(error) => error.fmt(formatter),
            Self::SourceUnavailable => {
                write!(formatter, "Nie można odczytać pliku fixture źródła.")
            }
            Self::TargetUnavailable => write!(formatter, "Nie można otworzyć pliku fixture celu."),
            Self::SourceMatchesTarget => write!(
                formatter,
                "Źródło i plik fixture celu są tym samym plikiem."
            ),
            Self::TargetIsNotRegularFile => {
                write!(formatter, "Silnik fixture wymaga zwykłego pliku celu.")
            }
            Self::TargetTooSmall => write!(formatter, "Plik fixture celu jest zbyt mały."),
            Self::CopyFailed => write!(formatter, "Nie udało się zapisać pliku fixture celu."),
            Self::VerificationReadFailed => {
                write!(formatter, "Nie udało się odczytać danych do weryfikacji.")
            }
            Self::VerificationMismatch => write!(
                formatter,
                "Weryfikacja odtworzonego pliku fixture nie powiodła się."
            ),
        }
    }
}

/// Copies the explicitly recognised RAWNAND region into a pre-sized regular
/// file fixture, then verifies it byte-for-byte. FULL NAND fixtures skip the
/// fixed BOOT0/BOOT1 prefix because the Linux main eMMC node exposes only the
/// RAWNAND address space. This function rejects block devices and is not a
/// production device writer.
pub fn restore_raw_nand_to_fixture(
    source_path: impl AsRef<Path>,
    target_path: impl AsRef<Path>,
    observer: &mut impl RestoreFixtureObserver,
) -> Result<RestoreFixtureReport, RestoreFixtureError> {
    let source_path = source_path.as_ref();
    let target_path = target_path.as_ref();
    if fs::canonicalize(source_path).ok() == fs::canonicalize(target_path).ok() {
        return Err(RestoreFixtureError::SourceMatchesTarget);
    }
    let target_metadata =
        fs::metadata(target_path).map_err(|_| RestoreFixtureError::TargetUnavailable)?;
    if !target_metadata.is_file() {
        return Err(RestoreFixtureError::TargetIsNotRegularFile);
    }
    let inspection = inspect_nand_image(source_path).map_err(RestoreFixtureError::Inspection)?;
    if target_metadata.len() < inspection.image_byte_len {
        return Err(RestoreFixtureError::TargetTooSmall);
    }
    let source =
        ImageReader::open(source_path).map_err(|_| RestoreFixtureError::SourceUnavailable)?;
    let mut target = OpenOptions::new()
        .read(true)
        .write(true)
        .open(target_path)
        .map_err(|_| RestoreFixtureError::TargetUnavailable)?;

    let total = inspection.image_byte_len;
    let mut buffer = vec![0_u8; RESTORE_FIXTURE_CHUNK_BYTES];
    let mut processed = 0_u64;
    if !observer.on_progress(RestoreFixtureProgress {
        phase: RestoreFixturePhase::Copying,
        processed_bytes: processed,
        total_bytes: total,
    }) {
        return Ok(cancelled_fixture_report(
            RestoreFixturePhase::Copying,
            processed,
            total,
        ));
    }
    while processed < total {
        let wanted = usize::try_from((total - processed).min(buffer.len() as u64))
            .expect("fixture chunk fits usize");
        source
            .read_exact_at(
                inspection.raw_nand_offset_bytes + processed,
                &mut buffer[..wanted],
            )
            .map_err(|_| RestoreFixtureError::SourceUnavailable)?;
        target
            .seek(SeekFrom::Start(processed))
            .and_then(|_| target.write_all(&buffer[..wanted]))
            .map_err(|_| RestoreFixtureError::CopyFailed)?;
        processed += wanted as u64;
        if !observer.on_progress(RestoreFixtureProgress {
            phase: RestoreFixturePhase::Copying,
            processed_bytes: processed,
            total_bytes: total,
        }) {
            return Ok(cancelled_fixture_report(
                RestoreFixturePhase::Copying,
                processed,
                total,
            ));
        }
    }
    target
        .sync_all()
        .map_err(|_| RestoreFixtureError::CopyFailed)?;

    processed = 0;
    if !observer.on_progress(RestoreFixtureProgress {
        phase: RestoreFixturePhase::Verifying,
        processed_bytes: processed,
        total_bytes: total,
    }) {
        return Ok(cancelled_fixture_report(
            RestoreFixturePhase::Verifying,
            processed,
            total,
        ));
    }
    let mut target_buffer = vec![0_u8; RESTORE_FIXTURE_CHUNK_BYTES];
    while processed < total {
        let wanted = usize::try_from((total - processed).min(buffer.len() as u64))
            .expect("fixture chunk fits usize");
        source
            .read_exact_at(
                inspection.raw_nand_offset_bytes + processed,
                &mut buffer[..wanted],
            )
            .map_err(|_| RestoreFixtureError::SourceUnavailable)?;
        target
            .seek(SeekFrom::Start(processed))
            .and_then(|_| target.read_exact(&mut target_buffer[..wanted]))
            .map_err(|_| RestoreFixtureError::VerificationReadFailed)?;
        if buffer[..wanted] != target_buffer[..wanted] {
            return Err(RestoreFixtureError::VerificationMismatch);
        }
        processed += wanted as u64;
        if !observer.on_progress(RestoreFixtureProgress {
            phase: RestoreFixturePhase::Verifying,
            processed_bytes: processed,
            total_bytes: total,
        }) {
            return Ok(cancelled_fixture_report(
                RestoreFixturePhase::Verifying,
                processed,
                total,
            ));
        }
    }
    Ok(RestoreFixtureReport {
        status: RestoreFixtureStatus::Completed,
        raw_nand_bytes: total,
        verified_bytes: total,
    })
}

/// Phases reported by the real-device raw restore transaction. The primary
/// GPT is deliberately a separate final commit: the payload is durable and
/// verified before the active partition map is changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreDevicePhase {
    CopyingPayload,
    VerifyingPayload,
    CommittingPrimaryMetadata,
    VerifyingPrimaryMetadata,
    RelocatingUser,
    WritingFatMirrors,
    CommittingBackupGpt,
    CommittingPrimaryGpt,
    CommittingBackupBootSector,
    CommittingPrimaryBootSector,
    VerifyingExpandedUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct RestoreDeviceProgress {
    pub phase: RestoreDevicePhase,
    pub processed_bytes: u64,
    pub total_bytes: u64,
}

/// Called only between write units and before the final metadata commit.
/// Returning `false` prevents the next write from starting. Once the primary
/// metadata commit has started it is intentionally no longer cancellable.
pub trait RestoreDeviceObserver {
    fn on_progress(&mut self, progress: RestoreDeviceProgress) -> bool;
}

pub struct ContinueRestoreDevice;

impl RestoreDeviceObserver for ContinueRestoreDevice {
    fn on_progress(&mut self, _: RestoreDeviceProgress) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreDeviceStatus {
    Completed,
    Cancelled {
        phase: RestoreDevicePhase,
        processed_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RestoreDeviceReport {
    pub status: RestoreDeviceStatus,
    pub raw_nand_bytes: u64,
    pub verified_bytes: u64,
}

#[derive(Debug)]
pub enum RestoreDeviceError {
    Inspection(InspectionError),
    TargetSafety(RestoreTargetSafetyError),
    UnsupportedMode,
    SourceMatchesTarget,
    TargetChanged,
    TargetTooSmall,
    TargetUnavailable,
    CopyFailed,
    VerificationReadFailed,
    VerificationMismatch,
    ResizePlanning,
    ResizeWriteFailed,
    ResizeVerificationFailed,
}

impl fmt::Display for RestoreDeviceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection(error) => error.fmt(formatter),
            Self::TargetSafety(error) => error.fmt(formatter),
            Self::UnsupportedMode => write!(
                formatter,
                "Writer urządzenia obsługuje obecnie wyłącznie zwykłe odtworzenie."
            ),
            Self::SourceMatchesTarget => {
                write!(
                    formatter,
                    "Źródło backupu i urządzenie docelowe są takie same."
                )
            }
            Self::TargetChanged => write!(
                formatter,
                "Urządzenie docelowe zmieniło się od czasu potwierdzenia."
            ),
            Self::TargetTooSmall => write!(formatter, "Urządzenie docelowe jest za małe."),
            Self::TargetUnavailable => {
                write!(
                    formatter,
                    "Nie można otworzyć urządzenia docelowego do zapisu."
                )
            }
            Self::CopyFailed => write!(formatter, "Zapis danych na urządzeniu nie powiódł się."),
            Self::VerificationReadFailed => {
                write!(formatter, "Nie można odczytać danych do weryfikacji.")
            }
            Self::VerificationMismatch => {
                write!(
                    formatter,
                    "Weryfikacja zapisu na urządzeniu nie powiodła się."
                )
            }
            Self::ResizePlanning => write!(
                formatter,
                "Nie można bezpiecznie zaplanować rozszerzenia USER."
            ),
            Self::ResizeWriteFailed => {
                write!(formatter, "Zapis fazy rozszerzenia USER nie powiódł się.")
            }
            Self::ResizeVerificationFailed => write!(
                formatter,
                "Weryfikacja rozszerzonego USER nie powiodła się."
            ),
        }
    }
}

/// Restores an explicitly recognised backup to an already authorised physical
/// device. The caller must first create `authorized` with
/// `authorize_restore_target`, which verifies the complete typed device path,
/// source/target inequality, mount state and a read-only exclusive lock.
///
/// This is intentionally limited to `OperationMode::Restore`. It writes and
/// verifies all sectors after the primary GPT region, synchronises them, then
/// writes and verifies the primary metadata as the final commit. It never
/// writes BOOT0/BOOT1 nodes; adapters that do not expose them require Hekate
/// for those areas.
pub fn restore_raw_nand_to_authorized_device(
    plan: &OperationPlan,
    authorized: &AuthorizedRestoreTarget,
    source_path: impl AsRef<Path>,
    observer: &mut impl RestoreDeviceObserver,
) -> Result<RestoreDeviceReport, RestoreDeviceError> {
    if plan.mode != OperationMode::Restore {
        return Err(RestoreDeviceError::UnsupportedMode);
    }
    let source_path = source_path.as_ref();
    if paths_name_same_device(source_path, Path::new(&authorized.canonical_path)) {
        return Err(RestoreDeviceError::SourceMatchesTarget);
    }
    let (current_path, current_target) =
        current_physical_target(plan).map_err(RestoreDeviceError::TargetSafety)?;
    if current_path != authorized.canonical_path || current_target.byte_len != authorized.byte_len {
        return Err(RestoreDeviceError::TargetChanged);
    }
    let target_name = Path::new(&current_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RestoreDeviceError::TargetChanged)?;
    if !mounted_at_for_target(target_name)
        .map_err(|_| RestoreDeviceError::TargetUnavailable)?
        .is_empty()
    {
        return Err(RestoreDeviceError::TargetSafety(
            RestoreTargetSafetyError::TargetMounted,
        ));
    }

    // Re-inspect immediately before opening the target read/write. This
    // protects the writer from a changed or incomplete source selected after
    // the original non-destructive plan was shown.
    let inspection = inspect_nand_image(source_path).map_err(RestoreDeviceError::Inspection)?;
    if inspection.image_byte_len > authorized.byte_len {
        return Err(RestoreDeviceError::TargetTooSmall);
    }
    let source = ImageReader::open(source_path).map_err(RestoreDeviceError::Inspection)?;
    let mut target = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&authorized.canonical_path)
        .map_err(|_| RestoreDeviceError::TargetUnavailable)?;
    device::ensure_same_authorized_device(authorized, &target)?;
    ensure_target_still_safe_for_write(plan, authorized)?;

    restore_raw_nand_to_open_target(&source, &inspection, &mut target, observer)
}

/// Restores an owner-supplied image and expands its encrypted FAT32 `USER`
/// partition on an already authorised physical device. The initial RAWNAND
/// restore is fully verified before any resize phase starts. Every following
/// phase is synced and read-back verified; the primary FAT32 boot sector is
/// the final, non-cancellable geometry commit.
pub fn restore_and_expand_user_to_authorized_device(
    plan: &OperationPlan,
    authorized: &AuthorizedRestoreTarget,
    source_path: impl AsRef<Path>,
    key: &BisKey,
    observer: &mut impl RestoreDeviceObserver,
) -> Result<RestoreDeviceReport, RestoreDeviceError> {
    if plan.mode != OperationMode::RestoreAndExpandUser {
        return Err(RestoreDeviceError::UnsupportedMode);
    }
    let source_path = source_path.as_ref();
    if paths_name_same_device(source_path, Path::new(&authorized.canonical_path)) {
        return Err(RestoreDeviceError::SourceMatchesTarget);
    }
    let (current_path, current_target) =
        current_physical_target(plan).map_err(RestoreDeviceError::TargetSafety)?;
    if current_path != authorized.canonical_path || current_target.byte_len != authorized.byte_len {
        return Err(RestoreDeviceError::TargetChanged);
    }
    let target_name = Path::new(&current_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RestoreDeviceError::TargetChanged)?;
    if !mounted_at_for_target(target_name)
        .map_err(|_| RestoreDeviceError::TargetUnavailable)?
        .is_empty()
    {
        return Err(RestoreDeviceError::TargetSafety(
            RestoreTargetSafetyError::TargetMounted,
        ));
    }

    // Complete source/key validation happens before the target is opened RW.
    let inspection = inspect_nand_image(source_path).map_err(RestoreDeviceError::Inspection)?;
    if inspection.image_byte_len > authorized.byte_len {
        return Err(RestoreDeviceError::TargetTooSmall);
    }
    let current_user_sectors = inspection
        .user_partition
        .last_lba
        .checked_sub(inspection.user_partition.first_lba)
        .and_then(|value| value.checked_add(1))
        .ok_or(RestoreDeviceError::ResizePlanning)?;
    plan_user_expansion(
        authorized.byte_len / LOGICAL_SECTOR_BYTES,
        inspection.user_partition.first_lba,
        current_user_sectors,
    )
    .map_err(|_| RestoreDeviceError::ResizePlanning)?;
    validate_user_fat32(
        source_path,
        inspection.raw_nand_offset_bytes,
        &inspection.user_partition,
        key,
    )
    .map_err(|_| RestoreDeviceError::ResizePlanning)?;

    let source = ImageReader::open(source_path).map_err(RestoreDeviceError::Inspection)?;
    let mut target = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&authorized.canonical_path)
        .map_err(|_| RestoreDeviceError::TargetUnavailable)?;
    device::ensure_same_authorized_device(authorized, &target)?;
    ensure_target_still_safe_for_write(plan, authorized)?;

    let copied = restore_raw_nand_to_open_target(&source, &inspection, &mut target, observer)?;
    if !matches!(copied.status, RestoreDeviceStatus::Completed) {
        return Ok(copied);
    }
    ensure_target_still_safe_for_write(plan, authorized)?;

    let capacity = authorized.byte_len / LOGICAL_SECTOR_BYTES;
    let resize_plan = build_fixture_resize_plan(&mut target, capacity, key)
        .map_err(|_| RestoreDeviceError::ResizePlanning)?;
    run_device_resize_phases(
        &mut target,
        &resize_plan,
        key,
        inspection.image_byte_len,
        observer,
    )
}

/// Creates a read-only-first plan for expanding the `USER` partition already
/// present on `target`. The caller must still supply and validate BIS Key 2
/// during preflight; neither this value nor the returned plan contains key
/// material.
pub fn create_in_place_expansion_plan(
    target: BlockDevice,
) -> Result<InPlaceExpansionPlan, RestoreTargetSafetyError> {
    if target.is_read_only {
        return Err(RestoreTargetSafetyError::TargetReadOnly);
    }
    Ok(InPlaceExpansionPlan {
        target,
        requires_read_only_preflight: true,
        write_operations_enabled: cfg!(target_os = "linux"),
    })
}

/// Reads the selected physical device without changing it and derives the
/// complete encrypted USER relocation plan. The same parser is used again
/// while the exclusive authorization lock is held before the device is opened
/// read/write.
pub fn preflight_in_place_user_expansion(
    plan: InPlaceExpansionPlan,
    key: &BisKey,
) -> Result<InPlaceExpansionPreflight, InPlaceExpansionError> {
    let (canonical_path, target) =
        current_physical_block(&plan.target).map_err(InPlaceExpansionError::TargetSafety)?;
    ensure_unmounted_target(&canonical_path).map_err(InPlaceExpansionError::TargetSafety)?;
    let mut device =
        File::open(canonical_path).map_err(|_| InPlaceExpansionError::TargetUnavailable)?;
    let resize =
        build_fixture_resize_plan(&mut device, target.byte_len / LOGICAL_SECTOR_BYTES, key)
            .map_err(|_| InPlaceExpansionError::Planning)?;
    Ok(in_place_preflight_from_resize_plan(plan, &resize))
}

/// Acquires the same target barrier as restore, without a backup-source
/// comparison because the selected device is intentionally the in-place
/// source. It is only a read-only locked authorization token.
pub fn authorize_in_place_expansion_target(
    plan: &InPlaceExpansionPlan,
    typed_confirmation: &str,
) -> Result<AuthorizedRestoreTarget, RestoreTargetSafetyError> {
    let (canonical_path, target) = current_physical_block(&plan.target)?;
    ensure_unmounted_target(&canonical_path)?;
    if typed_confirmation != canonical_path {
        return Err(RestoreTargetSafetyError::ConfirmationMismatch);
    }
    let file = File::open(&canonical_path).map_err(|_| RestoreTargetSafetyError::CannotLock)?;
    file.try_lock_exclusive()
        .map_err(|_| RestoreTargetSafetyError::CannotLock)?;
    let rechecked = (|| {
        let (current_path, current_target) = current_physical_block(&plan.target)?;
        if current_path != canonical_path || current_target.byte_len != target.byte_len {
            return Err(RestoreTargetSafetyError::TargetChanged);
        }
        ensure_unmounted_target(&current_path)
    })();
    if let Err(error) = rechecked {
        let _ = FileExt::unlock(&file);
        return Err(error);
    }
    Ok(AuthorizedRestoreTarget {
        file,
        canonical_path,
        byte_len: target.byte_len,
    })
}

/// Executes a previously preflighted in-place expansion. It rebuilds the
/// complete GPT/FAT/relocation plan once while holding the authorization lock
/// and once after opening the writable descriptor; thus no cached preflight
/// data can authorize a changed device.
pub fn expand_user_in_place_on_authorized_device(
    plan: &InPlaceExpansionPlan,
    authorized: &AuthorizedRestoreTarget,
    key: &BisKey,
    observer: &mut impl RestoreDeviceObserver,
) -> Result<InPlaceExpansionReport, InPlaceExpansionError> {
    ensure_in_place_target_still_safe(plan, authorized)?;
    let capacity = authorized.byte_len / LOGICAL_SECTOR_BYTES;
    let mut locked_read = File::open(&authorized.canonical_path)
        .map_err(|_| InPlaceExpansionError::TargetUnavailable)?;
    let initial = build_fixture_resize_plan(&mut locked_read, capacity, key)
        .map_err(|_| InPlaceExpansionError::Planning)?;

    let mut target = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&authorized.canonical_path)
        .map_err(|_| InPlaceExpansionError::TargetUnavailable)?;
    device::ensure_same_authorized_device(authorized, &target)
        .map_err(map_in_place_device_error)?;
    ensure_in_place_target_still_safe(plan, authorized)?;
    let resize = build_fixture_resize_plan(&mut target, capacity, key)
        .map_err(|_| InPlaceExpansionError::Planning)?;
    if resize.expansion != initial.expansion
        || resize.gpt.user_first_lba != initial.gpt.user_first_lba
    {
        return Err(InPlaceExpansionError::TargetChanged);
    }
    let report = run_device_resize_phases(&mut target, &resize, key, 0, observer)
        .map_err(map_in_place_device_error)?;
    Ok(InPlaceExpansionReport {
        status: report.status,
        current_user_sectors: resize.expansion.current_user_sectors,
        target_user_sectors: resize.expansion.target_user_sectors,
    })
}

fn in_place_preflight_from_resize_plan(
    plan: InPlaceExpansionPlan,
    resize: &FixtureResizePlan,
) -> InPlaceExpansionPreflight {
    InPlaceExpansionPreflight {
        plan,
        user_partition: GptPartition {
            name: "USER".to_owned(),
            first_lba: resize.gpt.user_first_lba,
            last_lba: resize.gpt.user_last_lba,
        },
        expanded_user: resize.expansion,
        user_fat32: resize.fat32.clone(),
        fat32_expansion: resize.fat32_expansion.clone(),
        relocation_count: resize.relocations.len(),
    }
}

fn ensure_in_place_target_still_safe(
    plan: &InPlaceExpansionPlan,
    authorized: &AuthorizedRestoreTarget,
) -> Result<(), InPlaceExpansionError> {
    let (current_path, current_target) =
        current_physical_block(&plan.target).map_err(InPlaceExpansionError::TargetSafety)?;
    if current_path != authorized.canonical_path || current_target.byte_len != authorized.byte_len {
        return Err(InPlaceExpansionError::TargetChanged);
    }
    ensure_unmounted_target(&current_path).map_err(InPlaceExpansionError::TargetSafety)
}

fn map_in_place_device_error(error: RestoreDeviceError) -> InPlaceExpansionError {
    match error {
        RestoreDeviceError::TargetSafety(error) => InPlaceExpansionError::TargetSafety(error),
        RestoreDeviceError::TargetChanged => InPlaceExpansionError::TargetChanged,
        RestoreDeviceError::TargetUnavailable => InPlaceExpansionError::TargetUnavailable,
        RestoreDeviceError::ResizeVerificationFailed | RestoreDeviceError::VerificationMismatch => {
            InPlaceExpansionError::VerificationFailed
        }
        _ => InPlaceExpansionError::WriteFailed,
    }
}

fn run_device_resize_phases(
    target: &mut File,
    plan: &FixtureResizePlan,
    key: &BisKey,
    raw_nand_bytes: u64,
    observer: &mut impl RestoreDeviceObserver,
) -> Result<RestoreDeviceReport, RestoreDeviceError> {
    let relocation_total = u64::try_from(plan.relocations.len())
        .ok()
        .and_then(|count| count.checked_mul(plan.cluster_byte_len as u64))
        .ok_or(RestoreDeviceError::ResizePlanning)?;
    if !begin_resize_phase(
        observer,
        RestoreDevicePhase::RelocatingUser,
        relocation_total,
    ) {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::RelocatingUser,
            0,
            raw_nand_bytes,
        ));
    }
    let mut relocated = 0_u64;
    for relocation in &plan.relocations {
        let mut cluster = vec![0_u8; plan.cluster_byte_len];
        read_user_plain_from_fixture(
            target,
            plan.user_offset,
            plan.current_user_byte_len,
            key,
            relocation.source_sector * LOGICAL_SECTOR_BYTES,
            &mut cluster,
        )
        .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
        write_user_plain_to_fixture(
            target,
            plan.user_offset,
            plan.target_user_byte_len,
            key,
            relocation.target_sector * LOGICAL_SECTOR_BYTES,
            &cluster,
        )
        .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
        let mut verified = vec![0_u8; plan.cluster_byte_len];
        read_user_plain_from_fixture(
            target,
            plan.user_offset,
            plan.target_user_byte_len,
            key,
            relocation.target_sector * LOGICAL_SECTOR_BYTES,
            &mut verified,
        )
        .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
        if cluster != verified {
            return Err(RestoreDeviceError::ResizeVerificationFailed);
        }
        relocated += plan.cluster_byte_len as u64;
        if !observer.on_progress(RestoreDeviceProgress {
            phase: RestoreDevicePhase::RelocatingUser,
            processed_bytes: relocated,
            total_bytes: relocation_total,
        }) {
            return Ok(cancelled_device_report(
                RestoreDevicePhase::RelocatingUser,
                relocated,
                raw_nand_bytes,
            ));
        }
    }
    sync_device_resize(target)?;

    if !begin_resize_phase(
        observer,
        RestoreDevicePhase::WritingFatMirrors,
        (plan.expanded_fat.len() * 2) as u64,
    ) {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::WritingFatMirrors,
            0,
            raw_nand_bytes,
        ));
    }
    write_user_plain_to_fixture(
        target,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        plan.fat_offset,
        &plan.expanded_fat,
    )
    .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
    write_user_plain_to_fixture(
        target,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        plan.target_mirror_offset,
        &plan.expanded_fat,
    )
    .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
    verify_fixture_user_plain(target, plan, key, plan.fat_offset, &plan.expanded_fat)
        .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    verify_fixture_user_plain(
        target,
        plan,
        key,
        plan.target_mirror_offset,
        &plan.expanded_fat,
    )
    .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    sync_device_resize(target)?;

    if !begin_resize_phase(observer, RestoreDevicePhase::CommittingBackupGpt, 1) {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::CommittingBackupGpt,
            0,
            raw_nand_bytes,
        ));
    }
    let backup_header_lba = u64::from_le_bytes(
        plan.backup_header[24..32]
            .try_into()
            .expect("GPT header field has fixed length"),
    );
    write_gpt_table_to_fixture(target, plan.backup_table_lba, &plan.primary_table)
        .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
    write_sector_to_fixture(target, backup_header_lba, &plan.backup_header)
        .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
    verify_gpt_copy_in_fixture(
        target,
        backup_header_lba,
        plan.backup_table_lba,
        &plan.backup_header,
        &plan.primary_table,
    )
    .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    sync_device_resize(target)?;
    let _ = observer.on_progress(RestoreDeviceProgress {
        phase: RestoreDevicePhase::CommittingBackupGpt,
        processed_bytes: 1,
        total_bytes: 1,
    });
    if !begin_resize_phase(observer, RestoreDevicePhase::CommittingPrimaryGpt, 1) {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::CommittingPrimaryGpt,
            0,
            raw_nand_bytes,
        ));
    }
    write_gpt_table_to_fixture(target, plan.gpt.entries_lba, &plan.primary_table)
        .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
    write_sector_to_fixture(target, GPT_HEADER_LBA, &plan.primary_header)
        .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
    verify_gpt_copy_in_fixture(
        target,
        GPT_HEADER_LBA,
        plan.gpt.entries_lba,
        &plan.primary_header,
        &plan.primary_table,
    )
    .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    sync_device_resize(target)?;

    if !begin_resize_phase(observer, RestoreDevicePhase::CommittingBackupBootSector, 1) {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::CommittingBackupBootSector,
            0,
            raw_nand_bytes,
        ));
    }
    write_user_plain_to_fixture(
        target,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        plan.backup_boot_offset,
        &plan.rewritten_boot,
    )
    .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
    verify_fixture_user_plain(
        target,
        plan,
        key,
        plan.backup_boot_offset,
        &plan.rewritten_boot,
    )
    .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    sync_device_resize(target)?;

    // This is the geometry commit. Cancellation is intentionally ignored from
    // here through the final read-back verification.
    let _ = observer.on_progress(RestoreDeviceProgress {
        phase: RestoreDevicePhase::CommittingPrimaryBootSector,
        processed_bytes: 0,
        total_bytes: 1,
    });
    write_user_plain_to_fixture(
        target,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        0,
        &plan.rewritten_boot,
    )
    .map_err(|_| RestoreDeviceError::ResizeWriteFailed)?;
    verify_fixture_user_plain(target, plan, key, 0, &plan.rewritten_boot)
        .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    sync_device_resize(target)?;

    let _ = observer.on_progress(RestoreDeviceProgress {
        phase: RestoreDevicePhase::VerifyingExpandedUser,
        processed_bytes: 0,
        total_bytes: 1,
    });
    verify_gpt_copy_in_fixture(
        target,
        GPT_HEADER_LBA,
        plan.gpt.entries_lba,
        &plan.primary_header,
        &plan.primary_table,
    )
    .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    verify_gpt_copy_in_fixture(
        target,
        backup_header_lba,
        plan.backup_table_lba,
        &plan.backup_header,
        &plan.primary_table,
    )
    .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    verify_fixture_user_plain(target, plan, key, plan.fat_offset, &plan.expanded_fat)
        .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    verify_fixture_user_plain(
        target,
        plan,
        key,
        plan.target_mirror_offset,
        &plan.expanded_fat,
    )
    .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    verify_fixture_user_plain(
        target,
        plan,
        key,
        plan.backup_boot_offset,
        &plan.rewritten_boot,
    )
    .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    verify_fixture_user_plain(target, plan, key, 0, &plan.rewritten_boot)
        .map_err(|_| RestoreDeviceError::ResizeVerificationFailed)?;
    let _ = observer.on_progress(RestoreDeviceProgress {
        phase: RestoreDevicePhase::VerifyingExpandedUser,
        processed_bytes: 1,
        total_bytes: 1,
    });
    Ok(RestoreDeviceReport {
        status: RestoreDeviceStatus::Completed,
        raw_nand_bytes,
        verified_bytes: raw_nand_bytes,
    })
}

fn begin_resize_phase(
    observer: &mut impl RestoreDeviceObserver,
    phase: RestoreDevicePhase,
    total_bytes: u64,
) -> bool {
    observer.on_progress(RestoreDeviceProgress {
        phase,
        processed_bytes: 0,
        total_bytes,
    })
}

fn sync_device_resize(target: &mut File) -> Result<(), RestoreDeviceError> {
    target
        .sync_all()
        .map_err(|_| RestoreDeviceError::ResizeWriteFailed)
}

fn ensure_target_still_safe_for_write(
    plan: &OperationPlan,
    authorized: &AuthorizedRestoreTarget,
) -> Result<(), RestoreDeviceError> {
    let (current_path, current_target) =
        current_physical_target(plan).map_err(RestoreDeviceError::TargetSafety)?;
    if current_path != authorized.canonical_path || current_target.byte_len != authorized.byte_len {
        return Err(RestoreDeviceError::TargetChanged);
    }
    let target_name = Path::new(&current_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RestoreDeviceError::TargetChanged)?;
    if !mounted_at_for_target(target_name)
        .map_err(|_| RestoreDeviceError::TargetUnavailable)?
        .is_empty()
    {
        return Err(RestoreDeviceError::TargetSafety(
            RestoreTargetSafetyError::TargetMounted,
        ));
    }
    Ok(())
}

fn restore_raw_nand_to_open_target(
    source: &ImageReader,
    inspection: &RawNandInspection,
    target: &mut File,
    observer: &mut impl RestoreDeviceObserver,
) -> Result<RestoreDeviceReport, RestoreDeviceError> {
    let total = inspection.image_byte_len;
    let metadata_len = inspection.primary_metadata_byte_len;
    if metadata_len == 0 || metadata_len > total {
        return Err(RestoreDeviceError::Inspection(InspectionError::InvalidGpt(
            "nieprawidłowy zakres primary GPT",
        )));
    }
    let payload_len = total - metadata_len;
    if !observer.on_progress(RestoreDeviceProgress {
        phase: RestoreDevicePhase::CopyingPayload,
        processed_bytes: 0,
        total_bytes: payload_len,
    }) {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::CopyingPayload,
            0,
            total,
        ));
    }
    if let Some(processed) = copy_source_range_to_target(
        source,
        inspection.raw_nand_offset_bytes,
        target,
        metadata_len,
        payload_len,
        RestoreDevicePhase::CopyingPayload,
        observer,
    )? {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::CopyingPayload,
            processed,
            total,
        ));
    }
    target
        .sync_all()
        .map_err(|_| RestoreDeviceError::CopyFailed)?;

    if !observer.on_progress(RestoreDeviceProgress {
        phase: RestoreDevicePhase::VerifyingPayload,
        processed_bytes: 0,
        total_bytes: payload_len,
    }) {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::VerifyingPayload,
            0,
            total,
        ));
    }
    if let Some(processed) = verify_source_range_on_target(
        source,
        inspection.raw_nand_offset_bytes,
        target,
        metadata_len,
        payload_len,
        RestoreDevicePhase::VerifyingPayload,
        observer,
    )? {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::VerifyingPayload,
            processed,
            total,
        ));
    }

    if !observer.on_progress(RestoreDeviceProgress {
        phase: RestoreDevicePhase::CommittingPrimaryMetadata,
        processed_bytes: 0,
        total_bytes: metadata_len,
    }) {
        return Ok(cancelled_device_report(
            RestoreDevicePhase::CommittingPrimaryMetadata,
            0,
            total,
        ));
    }
    copy_source_range_to_target_uninterruptibly(
        source,
        inspection.raw_nand_offset_bytes,
        target,
        0,
        metadata_len,
    )?;
    target
        .sync_all()
        .map_err(|_| RestoreDeviceError::CopyFailed)?;
    verify_source_range_on_target_uninterruptibly(
        source,
        inspection.raw_nand_offset_bytes,
        target,
        0,
        metadata_len,
    )?;
    let _ = observer.on_progress(RestoreDeviceProgress {
        phase: RestoreDevicePhase::VerifyingPrimaryMetadata,
        processed_bytes: metadata_len,
        total_bytes: metadata_len,
    });

    Ok(RestoreDeviceReport {
        status: RestoreDeviceStatus::Completed,
        raw_nand_bytes: total,
        verified_bytes: total,
    })
}

fn copy_source_range_to_target(
    source: &ImageReader,
    source_raw_offset: u64,
    target: &mut File,
    target_offset: u64,
    byte_len: u64,
    phase: RestoreDevicePhase,
    observer: &mut impl RestoreDeviceObserver,
) -> Result<Option<u64>, RestoreDeviceError> {
    let mut buffer = vec![0_u8; RESTORE_FIXTURE_CHUNK_BYTES];
    let mut processed = 0_u64;
    while processed < byte_len {
        let wanted = usize::try_from((byte_len - processed).min(buffer.len() as u64))
            .expect("restore chunk fits usize");
        source
            .read_exact_at(
                source_raw_offset + target_offset + processed,
                &mut buffer[..wanted],
            )
            .map_err(RestoreDeviceError::Inspection)?;
        target
            .seek(SeekFrom::Start(target_offset + processed))
            .and_then(|_| target.write_all(&buffer[..wanted]))
            .map_err(|_| RestoreDeviceError::CopyFailed)?;
        processed += wanted as u64;
        if !observer.on_progress(RestoreDeviceProgress {
            phase,
            processed_bytes: processed,
            total_bytes: byte_len,
        }) {
            return Ok(Some(processed));
        }
    }
    Ok(None)
}

fn verify_source_range_on_target(
    source: &ImageReader,
    source_raw_offset: u64,
    target: &mut File,
    target_offset: u64,
    byte_len: u64,
    phase: RestoreDevicePhase,
    observer: &mut impl RestoreDeviceObserver,
) -> Result<Option<u64>, RestoreDeviceError> {
    let mut source_buffer = vec![0_u8; RESTORE_FIXTURE_CHUNK_BYTES];
    let mut target_buffer = vec![0_u8; RESTORE_FIXTURE_CHUNK_BYTES];
    let mut processed = 0_u64;
    while processed < byte_len {
        let wanted = usize::try_from((byte_len - processed).min(source_buffer.len() as u64))
            .expect("restore chunk fits usize");
        source
            .read_exact_at(
                source_raw_offset + target_offset + processed,
                &mut source_buffer[..wanted],
            )
            .map_err(RestoreDeviceError::Inspection)?;
        target
            .seek(SeekFrom::Start(target_offset + processed))
            .and_then(|_| target.read_exact(&mut target_buffer[..wanted]))
            .map_err(|_| RestoreDeviceError::VerificationReadFailed)?;
        if source_buffer[..wanted] != target_buffer[..wanted] {
            return Err(RestoreDeviceError::VerificationMismatch);
        }
        processed += wanted as u64;
        if !observer.on_progress(RestoreDeviceProgress {
            phase,
            processed_bytes: processed,
            total_bytes: byte_len,
        }) {
            return Ok(Some(processed));
        }
    }
    Ok(None)
}

fn copy_source_range_to_target_uninterruptibly(
    source: &ImageReader,
    source_raw_offset: u64,
    target: &mut File,
    target_offset: u64,
    byte_len: u64,
) -> Result<(), RestoreDeviceError> {
    let mut buffer = vec![0_u8; RESTORE_FIXTURE_CHUNK_BYTES];
    let mut processed = 0_u64;
    while processed < byte_len {
        let wanted = usize::try_from((byte_len - processed).min(buffer.len() as u64))
            .expect("restore chunk fits usize");
        source
            .read_exact_at(
                source_raw_offset + target_offset + processed,
                &mut buffer[..wanted],
            )
            .map_err(RestoreDeviceError::Inspection)?;
        target
            .seek(SeekFrom::Start(target_offset + processed))
            .and_then(|_| target.write_all(&buffer[..wanted]))
            .map_err(|_| RestoreDeviceError::CopyFailed)?;
        processed += wanted as u64;
    }
    Ok(())
}

fn verify_source_range_on_target_uninterruptibly(
    source: &ImageReader,
    source_raw_offset: u64,
    target: &mut File,
    target_offset: u64,
    byte_len: u64,
) -> Result<(), RestoreDeviceError> {
    let mut source_buffer = vec![0_u8; RESTORE_FIXTURE_CHUNK_BYTES];
    let mut target_buffer = vec![0_u8; RESTORE_FIXTURE_CHUNK_BYTES];
    let mut processed = 0_u64;
    while processed < byte_len {
        let wanted = usize::try_from((byte_len - processed).min(source_buffer.len() as u64))
            .expect("restore chunk fits usize");
        source
            .read_exact_at(
                source_raw_offset + target_offset + processed,
                &mut source_buffer[..wanted],
            )
            .map_err(RestoreDeviceError::Inspection)?;
        target
            .seek(SeekFrom::Start(target_offset + processed))
            .and_then(|_| target.read_exact(&mut target_buffer[..wanted]))
            .map_err(|_| RestoreDeviceError::VerificationReadFailed)?;
        if source_buffer[..wanted] != target_buffer[..wanted] {
            return Err(RestoreDeviceError::VerificationMismatch);
        }
        processed += wanted as u64;
    }
    Ok(())
}

fn cancelled_device_report(
    phase: RestoreDevicePhase,
    processed_bytes: u64,
    raw_nand_bytes: u64,
) -> RestoreDeviceReport {
    RestoreDeviceReport {
        status: RestoreDeviceStatus::Cancelled {
            phase,
            processed_bytes,
        },
        raw_nand_bytes,
        verified_bytes: 0,
    }
}

fn cancelled_fixture_report(
    phase: RestoreFixturePhase,
    processed_bytes: u64,
    raw_nand_bytes: u64,
) -> RestoreFixtureReport {
    RestoreFixtureReport {
        status: RestoreFixtureStatus::Cancelled {
            phase,
            processed_bytes,
        },
        raw_nand_bytes,
        verified_bytes: 0,
    }
}

fn validate_boot_backups(
    format: NandImageFormat,
    boot0_path: Option<&Path>,
    boot1_path: Option<&Path>,
) -> Result<(BootComponentSource, BootComponentSource), BootBackupError> {
    match (format, boot0_path, boot1_path) {
        (NandImageFormat::FullNand, None, None) => {
            Ok((BootComponentSource::Embedded, BootComponentSource::Embedded))
        }
        (NandImageFormat::FullNand, _, _) => Err(BootBackupError::UnexpectedForFullNand),
        (NandImageFormat::RawNand, None, None) => {
            Ok((BootComponentSource::Absent, BootComponentSource::Absent))
        }
        (NandImageFormat::RawNand, Some(boot0), Some(boot1)) => {
            for path in [boot0, boot1] {
                let metadata = fs::metadata(path).map_err(|_| BootBackupError::Unavailable)?;
                if !metadata.is_file() {
                    return Err(BootBackupError::Unavailable);
                }
                if metadata.len() != BOOT_PARTITION_BYTES {
                    return Err(BootBackupError::IncorrectSize);
                }
            }
            Ok((BootComponentSource::Supplied, BootComponentSource::Supplied))
        }
        (NandImageFormat::RawNand, _, _) => Err(BootBackupError::IncompletePair),
    }
}

#[derive(Debug)]
pub enum InspectionError {
    Io(io::Error),
    ImageNotRegularFile,
    ImageTooSmall,
    InvalidGpt(&'static str),
    MissingUserPartition,
    MissingSplitPart,
}

#[derive(Debug)]
pub enum KeysetFileError {
    Unavailable,
    TooLarge,
    InvalidFormat(KeysetError),
}

impl fmt::Display for KeysetFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(formatter, "Keyset nie jest dostępny w tej sesji."),
            Self::TooLarge => write!(formatter, "Keyset przekracza bezpieczny limit rozmiaru."),
            Self::InvalidFormat(error) => error.fmt(formatter),
        }
    }
}

impl fmt::Display for InspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => write!(formatter, "Nie można odczytać obrazu."),
            Self::ImageNotRegularFile => write!(formatter, "Backup musi być zwykłym plikiem."),
            Self::ImageTooSmall => write!(formatter, "Obraz jest za mały, aby zawierać GPT."),
            Self::InvalidGpt(reason) => write!(formatter, "Nieprawidłowy GPT: {reason}"),
            Self::MissingUserPartition => write!(formatter, "W GPT nie znaleziono partycji USER."),
            Self::MissingSplitPart => write!(formatter, "Backup dzielony ma brakującą część."),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fat32BootSector {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub fat_count: u8,
    pub sectors_per_fat: u32,
    pub total_sectors: u32,
    pub media_descriptor: u8,
    pub backup_boot_sector: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fat32Inspection {
    pub boot_sector: Fat32BootSector,
    pub cluster_count: u32,
    pub allocated_clusters: u32,
    pub free_clusters: u32,
    pub bad_clusters: u32,
    pub chain_count: u32,
    pub largest_chain_clusters: u32,
}

/// One complete, non-overlapping FAT32 allocation chain. The parser returns
/// cluster numbers rather than filesystem paths: directory parsing is a later
/// validation layer, while relocation must preserve every allocated cluster,
/// including metadata clusters not reachable from a directory after damage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fat32Chain {
    pub first_cluster: u32,
    pub clusters: Vec<u32>,
}

/// A single data-cluster copy in the safe order for a FAT32 resize. Sectors are
/// relative to the start of USER; callers encrypt each 16 KiB cluster using
/// its old and new partition-relative data-unit index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fat32ClusterRelocation {
    pub cluster: u32,
    pub source_sector: u64,
    pub target_sector: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fat32ExpansionPlan {
    pub current_user_sectors: u32,
    pub target_user_sectors: u32,
    pub current_sectors_per_fat: u32,
    pub target_sectors_per_fat: u32,
    pub current_data_start_sector: u32,
    pub target_data_start_sector: u32,
    pub data_start_shift_sectors: u32,
    pub current_cluster_count: u32,
    pub target_cluster_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fat32PlanError {
    TargetNotLarger,
    InvalidGeometry,
    CapacityTooLarge,
    DidNotConverge,
}

#[derive(Debug)]
pub enum UserFilesystemError {
    CannotRead,
    UserTooSmall,
    InvalidBootSector(&'static str),
}

impl fmt::Display for UserFilesystemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CannotRead => {
                write!(formatter, "Nie można odczytać zaszyfrowanej partycji USER.")
            }
            Self::UserTooSmall => write!(
                formatter,
                "Partycja USER jest za mała dla jednostki kryptograficznej."
            ),
            Self::InvalidBootSector(_) => write!(
                formatter,
                "Keyset nie pasuje do USER albo boot sector FAT32 jest nieprawidłowy."
            ),
        }
    }
}

impl From<io::Error> for InspectionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct ImagePart {
    #[cfg(target_os = "windows")]
    path: PathBuf,
    file: Mutex<File>,
    start: u64,
    byte_len: u64,
}

struct ImageReader {
    parts: Vec<ImagePart>,
    byte_len: u64,
}

impl ImageReader {
    fn open(path: impl AsRef<Path>) -> Result<Self, InspectionError> {
        Self::open_with(path, false)
    }

    #[cfg(target_os = "windows")]
    fn open_locked(path: impl AsRef<Path>) -> Result<Self, InspectionError> {
        Self::open_with(path, true)
    }

    fn open_with(path: impl AsRef<Path>, lock_sources: bool) -> Result<Self, InspectionError> {
        let path = path.as_ref();
        let paths = split_paths(path)?;
        let mut parts = Vec::with_capacity(paths.len());
        let mut byte_len = 0_u64;
        for path in paths {
            if !fs::metadata(&path)?.is_file() {
                return Err(InspectionError::ImageNotRegularFile);
            }
            let file = if lock_sources {
                #[cfg(target_os = "windows")]
                {
                    open_source_file_locked(&path)?
                }
                #[cfg(not(target_os = "windows"))]
                {
                    File::open(&path)?
                }
            } else {
                File::open(&path)?
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(InspectionError::ImageNotRegularFile);
            }
            let part_len = metadata.len();
            if part_len == 0 {
                return Err(InspectionError::MissingSplitPart);
            }
            parts.push(ImagePart {
                #[cfg(target_os = "windows")]
                path,
                file: Mutex::new(file),
                start: byte_len,
                byte_len: part_len,
            });
            byte_len = byte_len
                .checked_add(part_len)
                .ok_or(InspectionError::ImageTooSmall)?;
        }
        Ok(Self { parts, byte_len })
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> Result<(), InspectionError> {
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or(InspectionError::ImageTooSmall)?;
        if end > self.byte_len {
            return Err(InspectionError::ImageTooSmall);
        }

        let mut current_offset = offset;
        let mut written = 0_usize;
        while written < output.len() {
            let part = self
                .parts
                .iter()
                .find(|part| {
                    current_offset >= part.start && current_offset < part.start + part.byte_len
                })
                .ok_or(InspectionError::MissingSplitPart)?;
            let in_part_offset = current_offset - part.start;
            let available = part.byte_len - in_part_offset;
            let wanted = (output.len() - written).min(available as usize);
            let mut file = part.file.lock().map_err(|_| {
                InspectionError::Io(io::Error::other("source handle lock poisoned"))
            })?;
            file.seek(SeekFrom::Start(in_part_offset))?;
            file.read_exact(&mut output[written..written + wanted])?;
            current_offset += wanted as u64;
            written += wanted;
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn open_source_file_locked(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    // Deny concurrent writes and renames for the entire lifetime of the
    // reader. All split parts remain open until the operation ends.
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

fn validate_user_fat32(
    path: impl AsRef<Path>,
    raw_nand_offset: u64,
    user_partition: &GptPartition,
    key: &BisKey,
) -> Result<Fat32Inspection, UserFilesystemError> {
    let image = ImageReader::open(path).map_err(|_| UserFilesystemError::CannotRead)?;
    validate_user_fat32_reader(&image, raw_nand_offset, user_partition, key)
}

fn validate_user_fat32_reader(
    image: &ImageReader,
    raw_nand_offset: u64,
    user_partition: &GptPartition,
    key: &BisKey,
) -> Result<Fat32Inspection, UserFilesystemError> {
    read_user_fat32_reader(image, raw_nand_offset, user_partition, key)
        .map(|(inspection, _)| inspection)
}

fn read_user_fat32_reader(
    image: &ImageReader,
    raw_nand_offset: u64,
    user_partition: &GptPartition,
    key: &BisKey,
) -> Result<(Fat32Inspection, Vec<u8>), UserFilesystemError> {
    let user_sectors = user_partition
        .last_lba
        .checked_sub(user_partition.first_lba)
        .and_then(|sectors| sectors.checked_add(1))
        .ok_or(UserFilesystemError::UserTooSmall)?;
    let partition_byte_len = user_sectors
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(UserFilesystemError::CannotRead)?;
    if partition_byte_len < BIS_DATA_UNIT_BYTES as u64 {
        return Err(UserFilesystemError::UserTooSmall);
    }
    let partition_offset = user_partition
        .first_lba
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .and_then(|offset| offset.checked_add(raw_nand_offset))
        .ok_or(UserFilesystemError::CannotRead)?;
    let reader = EncryptedUserReader {
        image,
        partition_offset,
        partition_byte_len,
        crypto: SwitchAesXts128::new(key),
    };
    let mut boot_sector_bytes = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    reader.read_exact_at(0, &mut boot_sector_bytes)?;
    let boot_sector = parse_fat32_boot_sector(&boot_sector_bytes, user_sectors)?;
    let mut backup_boot_sector = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    reader.read_exact_at(
        u64::from(boot_sector.backup_boot_sector) * LOGICAL_SECTOR_BYTES,
        &mut backup_boot_sector,
    )?;
    if backup_boot_sector != boot_sector_bytes {
        return Err(UserFilesystemError::InvalidBootSector(
            "zapasowy boot sector FAT32 różni się",
        ));
    }

    let fat_byte_len = u64::from(boot_sector.sectors_per_fat) * LOGICAL_SECTOR_BYTES;
    if fat_byte_len == 0 || fat_byte_len > MAX_FAT_BYTES {
        return Err(UserFilesystemError::InvalidBootSector(
            "tablica FAT ma nieobsługiwany rozmiar",
        ));
    }
    let fat_start = u64::from(boot_sector.reserved_sectors) * LOGICAL_SECTOR_BYTES;
    let second_fat_start = fat_start
        .checked_add(fat_byte_len)
        .ok_or(UserFilesystemError::CannotRead)?;
    let fat_byte_len = usize::try_from(fat_byte_len).map_err(|_| {
        UserFilesystemError::InvalidBootSector("tablica FAT przekracza limit procesu")
    })?;
    let mut primary_fat = vec![0_u8; fat_byte_len];
    let mut mirror_fat = vec![0_u8; fat_byte_len];
    reader.read_exact_at(fat_start, &mut primary_fat)?;
    reader.read_exact_at(second_fat_start, &mut mirror_fat)?;
    if primary_fat != mirror_fat {
        return Err(UserFilesystemError::InvalidBootSector(
            "kopie FAT różnią się",
        ));
    }
    Ok((inspect_fat32(&boot_sector, &primary_fat)?, primary_fat))
}

const MAX_FAT_BYTES: u64 = 64 * 1024 * 1024;

struct EncryptedUserReader<'a> {
    image: &'a ImageReader,
    partition_offset: u64,
    partition_byte_len: u64,
    crypto: SwitchAesXts128,
}

impl EncryptedUserReader<'_> {
    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> Result<(), UserFilesystemError> {
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or(UserFilesystemError::CannotRead)?;
        if end > self.partition_byte_len {
            return Err(UserFilesystemError::CannotRead);
        }

        let mut source_offset = offset;
        let mut written = 0_usize;
        while written < output.len() {
            let unit_index = source_offset / BIS_DATA_UNIT_BYTES as u64;
            let within_unit = (source_offset % BIS_DATA_UNIT_BYTES as u64) as usize;
            let wanted = (output.len() - written).min(BIS_DATA_UNIT_BYTES - within_unit);
            let unit_offset = self
                .partition_offset
                .checked_add(unit_index * BIS_DATA_UNIT_BYTES as u64)
                .ok_or(UserFilesystemError::CannotRead)?;
            let mut unit = vec![0_u8; BIS_DATA_UNIT_BYTES];
            self.image
                .read_exact_at(unit_offset, &mut unit)
                .map_err(|_| UserFilesystemError::CannotRead)?;
            self.crypto
                .decrypt_unit(unit_index, &mut unit)
                .expect("fixed 16 KiB input");
            output[written..written + wanted]
                .copy_from_slice(&unit[within_unit..within_unit + wanted]);
            source_offset += wanted as u64;
            written += wanted;
        }
        Ok(())
    }
}

fn parse_fat32_boot_sector(
    sector: &[u8],
    user_sectors: u64,
) -> Result<Fat32BootSector, UserFilesystemError> {
    if sector.len() != LOGICAL_SECTOR_BYTES as usize {
        return Err(UserFilesystemError::InvalidBootSector(
            "zły rozmiar sektora",
        ));
    }
    if sector[510..512] != [0x55, 0xaa] {
        return Err(UserFilesystemError::InvalidBootSector("brak sygnatury"));
    }
    let bytes_per_sector = u16::from_le_bytes([sector[11], sector[12]]);
    if bytes_per_sector != LOGICAL_SECTOR_BYTES as u16 {
        return Err(UserFilesystemError::InvalidBootSector(
            "nietypowy rozmiar sektora",
        ));
    }
    let sectors_per_cluster = sector[13];
    if sectors_per_cluster != USER_CLUSTER_SECTORS as u8 {
        return Err(UserFilesystemError::InvalidBootSector(
            "zły rozmiar klastra",
        ));
    }
    let reserved_sectors = u16::from_le_bytes([sector[14], sector[15]]);
    let fat_count = sector[16];
    let root_directory_entries = u16::from_le_bytes([sector[17], sector[18]]);
    let media_descriptor = sector[21];
    let total_sectors_16 = u16::from_le_bytes([sector[19], sector[20]]);
    let sectors_per_fat_16 = u16::from_le_bytes([sector[22], sector[23]]);
    let sectors_per_fat = u32::from_le_bytes([sector[36], sector[37], sector[38], sector[39]]);
    let total_sectors_32 = u32::from_le_bytes([sector[32], sector[33], sector[34], sector[35]]);
    let extended_flags = u16::from_le_bytes([sector[40], sector[41]]);
    let backup_boot_sector = u16::from_le_bytes([sector[50], sector[51]]);
    let total_sectors = if total_sectors_16 != 0 {
        total_sectors_16 as u32
    } else {
        total_sectors_32
    };

    if reserved_sectors == 0 || fat_count != 2 || root_directory_entries != 0 {
        return Err(UserFilesystemError::InvalidBootSector(
            "niepoprawny BPB FAT32",
        ));
    }
    if backup_boot_sector == 0 || backup_boot_sector >= reserved_sectors {
        return Err(UserFilesystemError::InvalidBootSector(
            "brak poprawnego zapasowego boot sectora FAT32",
        ));
    }
    if sectors_per_fat_16 != 0 || sectors_per_fat == 0 || total_sectors == 0 {
        return Err(UserFilesystemError::InvalidBootSector(
            "brak parametrów FAT32",
        ));
    }
    if extended_flags & 0x0080 != 0 {
        return Err(UserFilesystemError::InvalidBootSector(
            "wyłączone mirroring FAT",
        ));
    }
    if &sector[82..90] != b"FAT32   " {
        return Err(UserFilesystemError::InvalidBootSector(
            "brak identyfikatora FAT32",
        ));
    }
    let data_start = u64::from(reserved_sectors)
        .checked_add(u64::from(fat_count) * u64::from(sectors_per_fat))
        .ok_or(UserFilesystemError::InvalidBootSector("przepełnienie BPB"))?;
    if data_start >= u64::from(total_sectors) || u64::from(total_sectors) != user_sectors {
        return Err(UserFilesystemError::InvalidBootSector(
            "rozmiar FAT32 nie odpowiada USER",
        ));
    }

    Ok(Fat32BootSector {
        bytes_per_sector,
        sectors_per_cluster,
        reserved_sectors,
        fat_count,
        sectors_per_fat,
        total_sectors,
        media_descriptor,
        backup_boot_sector,
    })
}

fn inspect_fat32(
    boot_sector: &Fat32BootSector,
    fat: &[u8],
) -> Result<Fat32Inspection, UserFilesystemError> {
    if fat.len() % 4 != 0 {
        return Err(UserFilesystemError::InvalidBootSector(
            "tablica FAT nie jest wyrównana do wpisu",
        ));
    }
    let first_entry = u32::from_le_bytes(fat[0..4].try_into().expect("fixed FAT entry"));
    let second_entry = u32::from_le_bytes(fat[4..8].try_into().expect("fixed FAT entry"));
    if first_entry & 0xff != u32::from(boot_sector.media_descriptor)
        || first_entry & 0x0fff_ff00 != 0x0fff_ff00
        || second_entry & 0x0fff_fff8 != 0x0fff_fff8
    {
        return Err(UserFilesystemError::InvalidBootSector(
            "zarezerwowane wpisy FAT są nieprawidłowe",
        ));
    }

    let data_start = u64::from(boot_sector.reserved_sectors)
        + u64::from(boot_sector.fat_count) * u64::from(boot_sector.sectors_per_fat);
    let cluster_count = (u64::from(boot_sector.total_sectors) - data_start)
        / u64::from(boot_sector.sectors_per_cluster);
    let cluster_count = u32::try_from(cluster_count).map_err(|_| {
        UserFilesystemError::InvalidBootSector("liczba klastrów przekracza limit FAT32")
    })?;
    let required_entries = usize::try_from(cluster_count)
        .ok()
        .and_then(|count| count.checked_add(2))
        .ok_or(UserFilesystemError::InvalidBootSector(
            "przepełnienie wpisów FAT",
        ))?;
    if fat.len() / 4 < required_entries {
        return Err(UserFilesystemError::InvalidBootSector(
            "tablica FAT jest za krótka",
        ));
    }

    let last_cluster =
        cluster_count
            .checked_add(1)
            .ok_or(UserFilesystemError::InvalidBootSector(
                "przepełnienie numeru klastra",
            ))?;
    let mut allocated_clusters = 0_u32;
    let mut free_clusters = 0_u32;
    let mut bad_clusters = 0_u32;
    for cluster in 2..=last_cluster {
        let offset = cluster as usize * 4;
        let value =
            u32::from_le_bytes(fat[offset..offset + 4].try_into().expect("fixed FAT entry"))
                & 0x0fff_ffff;
        match value {
            0 => free_clusters += 1,
            0x0fff_fff7 => bad_clusters += 1,
            2..=0x0fff_ffef if value <= last_cluster => allocated_clusters += 1,
            0x0fff_fff8..=0x0fff_ffff => allocated_clusters += 1,
            _ => {
                return Err(UserFilesystemError::InvalidBootSector(
                    "tablica FAT zawiera zarezerwowany wpis",
                ));
            }
        }
    }
    let chains = parse_fat32_chains(boot_sector, fat)?;
    let chain_clusters = chains.iter().try_fold(0_u32, |total, chain| {
        total.checked_add(chain.clusters.len() as u32).ok_or(
            UserFilesystemError::InvalidBootSector("przepełnienie długości łańcuchów FAT"),
        )
    })?;
    if chain_clusters != allocated_clusters {
        return Err(UserFilesystemError::InvalidBootSector(
            "łańcuchy FAT nie obejmują wszystkich zajętych klastrów",
        ));
    }
    let largest_chain_clusters = chains
        .iter()
        .map(|chain| chain.clusters.len() as u32)
        .max()
        .unwrap_or(0);

    Ok(Fat32Inspection {
        boot_sector: boot_sector.clone(),
        cluster_count,
        allocated_clusters,
        free_clusters,
        bad_clusters,
        chain_count: chains.len() as u32,
        largest_chain_clusters,
    })
}

/// Parses every allocated FAT32 chain and rejects ambiguous allocation graphs.
/// A valid allocation graph is a forest of singly-linked chains ending at EOC:
/// a cluster cannot have two predecessors, a link cannot target a free or bad
/// cluster and no allocated cluster may remain in a cycle.
pub fn parse_fat32_chains(
    boot_sector: &Fat32BootSector,
    fat: &[u8],
) -> Result<Vec<Fat32Chain>, UserFilesystemError> {
    let cluster_count = fat32_cluster_count(boot_sector)?;
    let last_cluster =
        cluster_count
            .checked_add(1)
            .ok_or(UserFilesystemError::InvalidBootSector(
                "przepełnienie numeru klastra",
            ))?;
    let required_entries = usize::try_from(last_cluster)
        .ok()
        .and_then(|last| last.checked_add(1))
        .ok_or(UserFilesystemError::InvalidBootSector(
            "przepełnienie wpisów FAT",
        ))?;
    if fat.len() / 4 < required_entries {
        return Err(UserFilesystemError::InvalidBootSector(
            "tablica FAT jest za krótka",
        ));
    }

    let mut incoming = vec![0_u8; required_entries];
    for cluster in 2..=last_cluster {
        let value = fat_entry(fat, cluster)?;
        match value {
            0 | 0x0fff_fff7 | 0x0fff_fff8..=0x0fff_ffff => {}
            2..=0x0fff_ffef if value <= last_cluster => {
                let target = usize::try_from(value).map_err(|_| {
                    UserFilesystemError::InvalidBootSector("nieprawidłowy cel łańcucha FAT")
                })?;
                incoming[target] = incoming[target].checked_add(1).ok_or(
                    UserFilesystemError::InvalidBootSector("cross-link w łańcuchu FAT"),
                )?;
                if incoming[target] > 1 {
                    return Err(UserFilesystemError::InvalidBootSector(
                        "cross-link w łańcuchu FAT",
                    ));
                }
            }
            _ => {
                return Err(UserFilesystemError::InvalidBootSector(
                    "tablica FAT zawiera zarezerwowany wpis",
                ));
            }
        }
    }

    let mut visited = vec![false; required_entries];
    let mut chains = Vec::new();
    for first_cluster in 2..=last_cluster {
        if fat_entry(fat, first_cluster)? == 0 || fat_entry(fat, first_cluster)? == 0x0fff_fff7 {
            continue;
        }
        if incoming[first_cluster as usize] != 0 {
            continue;
        }

        let mut clusters = Vec::new();
        let mut cluster = first_cluster;
        loop {
            if visited[cluster as usize] {
                return Err(UserFilesystemError::InvalidBootSector(
                    "cykl w łańcuchu FAT",
                ));
            }
            visited[cluster as usize] = true;
            clusters.push(cluster);
            let value = fat_entry(fat, cluster)?;
            match value {
                0x0fff_fff8..=0x0fff_ffff => break,
                2..=0x0fff_ffef if value <= last_cluster => {
                    let target_value = fat_entry(fat, value)?;
                    if target_value == 0 || target_value == 0x0fff_fff7 {
                        return Err(UserFilesystemError::InvalidBootSector(
                            "łańcuch FAT wskazuje wolny lub uszkodzony klaster",
                        ));
                    }
                    cluster = value;
                }
                _ => {
                    return Err(UserFilesystemError::InvalidBootSector(
                        "nieprawidłowe zakończenie łańcucha FAT",
                    ));
                }
            }
        }
        chains.push(Fat32Chain {
            first_cluster,
            clusters,
        });
    }

    for cluster in 2..=last_cluster {
        let value = fat_entry(fat, cluster)?;
        if value != 0 && value != 0x0fff_fff7 && !visited[cluster as usize] {
            return Err(UserFilesystemError::InvalidBootSector(
                "cykl w łańcuchu FAT",
            ));
        }
    }
    Ok(chains)
}

/// Produces the data moves for a previously calculated FAT32 expansion. The
/// entries are sorted descending by cluster number, which is the required
/// direction when source and destination may overlap on an already restored
/// image.
pub fn plan_fat32_cluster_relocations(
    boot_sector: &Fat32BootSector,
    fat: &[u8],
    expansion: &Fat32ExpansionPlan,
) -> Result<Vec<Fat32ClusterRelocation>, UserFilesystemError> {
    if boot_sector.sectors_per_cluster as u64 != USER_CLUSTER_SECTORS
        || expansion.current_data_start_sector
            != u32::from(boot_sector.reserved_sectors)
                + u32::from(boot_sector.fat_count) * boot_sector.sectors_per_fat
    {
        return Err(UserFilesystemError::InvalidBootSector(
            "geometria USER nie odpowiada jednostkom relokacji 16 KiB",
        ));
    }
    let mut clusters = parse_fat32_chains(boot_sector, fat)?
        .into_iter()
        .flat_map(|chain| chain.clusters)
        .collect::<Vec<_>>();
    clusters.sort_unstable_by(|left, right| right.cmp(left));

    let cluster_sectors = u64::from(boot_sector.sectors_per_cluster);
    clusters
        .into_iter()
        .map(|cluster| {
            let cluster_offset = u64::from(cluster - 2).checked_mul(cluster_sectors).ok_or(
                UserFilesystemError::InvalidBootSector("przepełnienie offsetu klastra"),
            )?;
            Ok(Fat32ClusterRelocation {
                cluster,
                source_sector: u64::from(expansion.current_data_start_sector)
                    .checked_add(cluster_offset)
                    .ok_or(UserFilesystemError::InvalidBootSector(
                        "przepełnienie offsetu klastra",
                    ))?,
                target_sector: u64::from(expansion.target_data_start_sector)
                    .checked_add(cluster_offset)
                    .ok_or(UserFilesystemError::InvalidBootSector(
                        "przepełnienie offsetu klastra",
                    ))?,
            })
        })
        .collect()
}

/// Expands one decrypted FAT copy in memory. Existing entries are kept byte for
/// byte and newly addressable entries are zeroed (free). The caller writes the
/// returned bytes to both FAT mirrors only after all planned data moves have
/// completed and been verified.
pub fn build_expanded_fat(
    fat: &[u8],
    expansion: &Fat32ExpansionPlan,
) -> Result<Vec<u8>, UserFilesystemError> {
    let current_byte_len = u64::from(expansion.current_sectors_per_fat)
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(UserFilesystemError::InvalidBootSector(
            "przepełnienie obecnej długości FAT",
        ))?;
    if fat.len() as u64 != current_byte_len {
        return Err(UserFilesystemError::InvalidBootSector(
            "długość FAT nie odpowiada geometrii",
        ));
    }
    let target_byte_len = u64::from(expansion.target_sectors_per_fat)
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(UserFilesystemError::InvalidBootSector(
            "przepełnienie docelowej długości FAT",
        ))?;
    if target_byte_len > MAX_FAT_BYTES {
        return Err(UserFilesystemError::InvalidBootSector(
            "docelowa FAT przekracza limit procesu",
        ));
    }
    let target_len = usize::try_from(target_byte_len).map_err(|_| {
        UserFilesystemError::InvalidBootSector("docelowa FAT przekracza limit procesu")
    })?;
    let mut expanded = vec![0_u8; target_len];
    expanded[..fat.len()].copy_from_slice(fat);
    Ok(expanded)
}

/// Produces the updated, decrypted FAT32 boot sector. It intentionally changes
/// only BPB fields describing FAT length and total partition sectors; callers
/// apply it to the primary and backup boot sectors as the final metadata
/// commit.
pub fn rewrite_fat32_boot_sector(
    sector: &[u8],
    expansion: &Fat32ExpansionPlan,
) -> Result<[u8; LOGICAL_SECTOR_BYTES as usize], UserFilesystemError> {
    if sector.len() != LOGICAL_SECTOR_BYTES as usize {
        return Err(UserFilesystemError::InvalidBootSector(
            "boot sector ma nieprawidłową długość",
        ));
    }
    let current = parse_fat32_boot_sector(sector, u64::from(expansion.current_user_sectors))?;
    if current.sectors_per_fat != expansion.current_sectors_per_fat
        || u32::from(current.reserved_sectors)
            + u32::from(current.fat_count) * current.sectors_per_fat
            != expansion.current_data_start_sector
    {
        return Err(UserFilesystemError::InvalidBootSector(
            "boot sector nie odpowiada planowi rozszerzenia",
        ));
    }
    let mut rewritten = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    rewritten.copy_from_slice(sector);
    // FAT32's 16-bit total-sector field must be zero when the 32-bit field is
    // authoritative. Clearing it also makes a small synthetic source safe to
    // grow past 65,535 sectors.
    rewritten[19..21].fill(0);
    rewritten[32..36].copy_from_slice(&expansion.target_user_sectors.to_le_bytes());
    rewritten[36..40].copy_from_slice(&expansion.target_sectors_per_fat.to_le_bytes());
    Ok(rewritten)
}

/// Checkpoints before every durable resize phase. The in-memory model returns
/// the untouched restored layout; the fixture transaction uses them to stop
/// after synchronising all preceding phases and before the next one begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAndExpandCheckpoint {
    BeforeClusterRelocation,
    BeforeFatMirrors,
    BeforeBackupGpt,
    BeforePrimaryGpt,
    BeforeBackupBootSector,
    BeforePrimaryBootSector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticRestoreStatus {
    Completed,
    InterruptedBefore(RestoreAndExpandCheckpoint),
}

/// Read-only conclusion drawn from the durable resize fixture after a failed
/// run or a simulated process restart. It deliberately never implies that an
/// automatic repair is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureRecoveryDisposition {
    /// The primary boot sector still describes the previous USER geometry.
    PreviousGeometryActive,
    /// Both GPT copies and both boot sectors describe the same new geometry.
    Committed,
    /// The primary boot sector is unusable but a self-consistent backup boot
    /// sector was found. Recovery must remain a manual, separately confirmed
    /// procedure.
    ManualRecoveryFromBackupBoot,
    /// The fixture does not contain enough mutually consistent metadata to
    /// state which geometry is active.
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureRecoveryGptCopy {
    Valid {
        user_first_lba: u64,
        user_last_lba: u64,
    },
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureRecoveryBootCopy {
    Valid {
        user_first_lba: u64,
        user_sectors: u64,
        fat_mirrors_match: bool,
    },
    Invalid,
}

/// A report produced by an independent, read-only parser. It contains no BIS
/// material and does not open a fixture for writing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FixtureRecoveryReport {
    pub capacity_sectors: u64,
    pub primary_gpt: FixtureRecoveryGptCopy,
    pub backup_gpt: FixtureRecoveryGptCopy,
    pub primary_boot: FixtureRecoveryBootCopy,
    pub backup_boot: FixtureRecoveryBootCopy,
    pub disposition: FixtureRecoveryDisposition,
}

#[derive(Debug)]
pub enum FixtureRecoveryError {
    TargetUnavailable,
    TargetIsNotRegularFile,
    TargetUnsupportedSize,
}

impl fmt::Display for FixtureRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetUnavailable => {
                write!(formatter, "Nie można otworzyć pliku fixture do odczytu.")
            }
            Self::TargetIsNotRegularFile => {
                write!(formatter, "Raport recovery wymaga zwykłego pliku fixture.")
            }
            Self::TargetUnsupportedSize => {
                write!(
                    formatter,
                    "Plik fixture musi kończyć się na granicy sektora."
                )
            }
        }
    }
}

/// Result of the synthetic restore+resize model. `raw_nand` is deliberately
/// an in-memory image: this API never opens a block device or a file for
/// writing. The device-writing transaction is a separate, later layer.
#[derive(Debug)]
pub struct SyntheticRestoreOutcome {
    pub raw_nand: Vec<u8>,
    pub status: SyntheticRestoreStatus,
}

#[derive(Debug)]
pub enum SyntheticRestoreError {
    Inspection(InspectionError),
    UserFilesystem(UserFilesystemError),
    Fat32Plan(Fat32PlanError),
    Expansion(PlanError),
    TargetTooSmall,
    TargetTooLarge,
}

struct ResizeGpt {
    header: [u8; LOGICAL_SECTOR_BYTES as usize],
    header_size: usize,
    table: Vec<u8>,
    entries_lba: u64,
    user_entry_offset: usize,
    user_first_lba: u64,
    user_last_lba: u64,
}

/// Applies the complete restore+resize transformation to a synthetic image.
///
/// The input can be RAWNAND or the explicit `BOOT0 || BOOT1 || RAWNAND`
/// container. In the latter case the result is the RAWNAND address space,
/// matching the Linux main eMMC device; BOOT areas are intentionally outside
/// this model. All transformations are prepared in a private working image.
/// Therefore an injected interruption returns a byte-for-byte restored source
/// image with its old FAT and GPT layout still readable.
pub fn restore_and_expand_user_image(
    source: &[u8],
    target_capacity_sectors: u64,
    key: &BisKey,
    interrupt_before: Option<RestoreAndExpandCheckpoint>,
) -> Result<SyntheticRestoreOutcome, SyntheticRestoreError> {
    let raw = raw_nand_from_image_bytes(source).map_err(SyntheticRestoreError::Inspection)?;
    let gpt = parse_resize_gpt(raw).map_err(SyntheticRestoreError::Inspection)?;
    let source_sectors =
        u64::try_from(raw.len() / LOGICAL_SECTOR_BYTES as usize).expect("usize always fits u64");
    if target_capacity_sectors < source_sectors {
        return Err(SyntheticRestoreError::TargetTooSmall);
    }
    let target_byte_len = target_capacity_sectors
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(SyntheticRestoreError::TargetTooLarge)?;
    if target_byte_len > MAX_SYNTHETIC_IMAGE_BYTES {
        return Err(SyntheticRestoreError::TargetTooLarge);
    }
    let target_len =
        usize::try_from(target_byte_len).map_err(|_| SyntheticRestoreError::TargetTooLarge)?;

    let current_user_sectors = gpt
        .user_last_lba
        .checked_sub(gpt.user_first_lba)
        .and_then(|value| value.checked_add(1))
        .ok_or(SyntheticRestoreError::Inspection(
            InspectionError::InvalidGpt("nieprawidłowy zakres USER"),
        ))?;
    let user_expansion = plan_user_expansion(
        target_capacity_sectors,
        gpt.user_first_lba,
        current_user_sectors,
    )
    .map_err(SyntheticRestoreError::Expansion)?;

    let mut restored = vec![0_u8; target_len];
    restored[..raw.len()].copy_from_slice(raw);
    let mut working = restored.clone();
    let user_offset = gpt
        .user_first_lba
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(SyntheticRestoreError::TargetTooLarge)?;
    let user_byte_len = current_user_sectors
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(SyntheticRestoreError::TargetTooLarge)?;

    let mut boot_sector = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    read_user_plain(
        &working,
        user_offset,
        user_byte_len,
        key,
        0,
        &mut boot_sector,
    )?;
    let boot = parse_fat32_boot_sector(&boot_sector, current_user_sectors)
        .map_err(SyntheticRestoreError::UserFilesystem)?;
    let mut backup_boot_sector = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    read_user_plain(
        &working,
        user_offset,
        user_byte_len,
        key,
        u64::from(boot.backup_boot_sector) * LOGICAL_SECTOR_BYTES,
        &mut backup_boot_sector,
    )?;
    if boot_sector != backup_boot_sector {
        return Err(SyntheticRestoreError::UserFilesystem(
            UserFilesystemError::InvalidBootSector("zapasowy boot sector FAT32 różni się"),
        ));
    }
    let fat_len = usize::try_from(u64::from(boot.sectors_per_fat) * LOGICAL_SECTOR_BYTES)
        .map_err(|_| SyntheticRestoreError::TargetTooLarge)?;
    let fat_offset = u64::from(boot.reserved_sectors) * LOGICAL_SECTOR_BYTES;
    let mut fat = vec![0_u8; fat_len];
    let mut mirror = vec![0_u8; fat_len];
    read_user_plain(
        &working,
        user_offset,
        user_byte_len,
        key,
        fat_offset,
        &mut fat,
    )?;
    read_user_plain(
        &working,
        user_offset,
        user_byte_len,
        key,
        fat_offset + fat_len as u64,
        &mut mirror,
    )?;
    if fat != mirror {
        return Err(SyntheticRestoreError::UserFilesystem(
            UserFilesystemError::InvalidBootSector("kopie FAT różnią się"),
        ));
    }
    inspect_fat32(&boot, &fat).map_err(SyntheticRestoreError::UserFilesystem)?;
    let fat_expansion = plan_fat32_expansion(&boot, user_expansion.target_user_sectors)
        .map_err(SyntheticRestoreError::Fat32Plan)?;
    let relocations = plan_fat32_cluster_relocations(&boot, &fat, &fat_expansion)
        .map_err(SyntheticRestoreError::UserFilesystem)?;

    if let Some(outcome) = interrupted_image(
        &restored,
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforeClusterRelocation,
    ) {
        return Ok(outcome);
    }
    let cluster_byte_len = u64::from(boot.sectors_per_cluster) * LOGICAL_SECTOR_BYTES;
    for relocation in relocations {
        let mut cluster = vec![0_u8; cluster_byte_len as usize];
        read_user_plain(
            &working,
            user_offset,
            user_byte_len,
            key,
            relocation.source_sector * LOGICAL_SECTOR_BYTES,
            &mut cluster,
        )?;
        write_user_plain(
            &mut working,
            user_offset,
            user_expansion.target_user_sectors * LOGICAL_SECTOR_BYTES,
            key,
            relocation.target_sector * LOGICAL_SECTOR_BYTES,
            &cluster,
        )?;
    }

    if let Some(outcome) = interrupted_image(
        &restored,
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforeFatMirrors,
    ) {
        return Ok(outcome);
    }
    let expanded_fat =
        build_expanded_fat(&fat, &fat_expansion).map_err(SyntheticRestoreError::UserFilesystem)?;
    write_user_plain(
        &mut working,
        user_offset,
        user_expansion.target_user_sectors * LOGICAL_SECTOR_BYTES,
        key,
        fat_offset,
        &expanded_fat,
    )?;
    let target_mirror_offset = fat_offset
        .checked_add(expanded_fat.len() as u64)
        .ok_or(SyntheticRestoreError::TargetTooLarge)?;
    write_user_plain(
        &mut working,
        user_offset,
        user_expansion.target_user_sectors * LOGICAL_SECTOR_BYTES,
        key,
        target_mirror_offset,
        &expanded_fat,
    )?;

    if let Some(outcome) = interrupted_image(
        &restored,
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforeBackupGpt,
    ) {
        return Ok(outcome);
    }
    let target_user_last_lba = gpt
        .user_first_lba
        .checked_add(user_expansion.target_user_sectors)
        .and_then(|value| value.checked_sub(1))
        .ok_or(SyntheticRestoreError::TargetTooLarge)?;
    let (primary_header, primary_table, backup_header, backup_table_lba) =
        expanded_gpt_metadata(&gpt, target_capacity_sectors, target_user_last_lba)?;
    write_gpt_table(&mut working, backup_table_lba, &primary_table)?;
    write_sector(&mut working, target_capacity_sectors - 1, &backup_header)?;

    if let Some(outcome) = interrupted_image(
        &restored,
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforePrimaryGpt,
    ) {
        return Ok(outcome);
    }
    write_gpt_table(&mut working, gpt.entries_lba, &primary_table)?;
    write_sector(&mut working, GPT_HEADER_LBA, &primary_header)?;

    let rewritten_boot = rewrite_fat32_boot_sector(&boot_sector, &fat_expansion)
        .map_err(SyntheticRestoreError::UserFilesystem)?;
    if let Some(outcome) = interrupted_image(
        &restored,
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforeBackupBootSector,
    ) {
        return Ok(outcome);
    }
    write_user_plain(
        &mut working,
        user_offset,
        user_expansion.target_user_sectors * LOGICAL_SECTOR_BYTES,
        key,
        u64::from(boot.backup_boot_sector) * LOGICAL_SECTOR_BYTES,
        &rewritten_boot,
    )?;
    if let Some(outcome) = interrupted_image(
        &restored,
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforePrimaryBootSector,
    ) {
        return Ok(outcome);
    }
    write_user_plain(
        &mut working,
        user_offset,
        user_expansion.target_user_sectors * LOGICAL_SECTOR_BYTES,
        key,
        0,
        &rewritten_boot,
    )?;

    Ok(SyntheticRestoreOutcome {
        raw_nand: working,
        status: SyntheticRestoreStatus::Completed,
    })
}

/// Runs a durable, phased restore+resize transaction against a pre-sized
/// regular-file fixture. It copies the source first, then reads the plan from
/// the fixture and persists every phase before continuing. It keeps only a
/// data unit, the FAT and GPT metadata in memory--never a full output image.
///
/// This is deliberately not a block-device writer. `interrupt_before` stops
/// before the named durable phase, after all earlier phases have been synced.
/// It exists to exercise recovery boundaries on synthetic fixtures.
pub fn restore_and_expand_user_to_fixture(
    source_path: impl AsRef<Path>,
    target_path: impl AsRef<Path>,
    key: &BisKey,
    interrupt_before: Option<RestoreAndExpandCheckpoint>,
) -> Result<SyntheticRestoreStatus, RestoreResizeFixtureError> {
    let source_path = source_path.as_ref();
    let target_path = target_path.as_ref();
    if fs::canonicalize(source_path).ok() == fs::canonicalize(target_path).ok() {
        return Err(RestoreResizeFixtureError::SourceMatchesTarget);
    }
    let target_metadata =
        fs::metadata(target_path).map_err(|_| RestoreResizeFixtureError::TargetUnavailable)?;
    if !target_metadata.is_file() {
        return Err(RestoreResizeFixtureError::TargetIsNotRegularFile);
    }
    if target_metadata.len() % LOGICAL_SECTOR_BYTES != 0 {
        return Err(RestoreResizeFixtureError::TargetUnsupportedSize);
    }

    // Validate the source and key before the target is touched. The later
    // plan is rebuilt from the verified copy, rather than trusted across the
    // copy boundary.
    let source_inspection =
        inspect_nand_image(source_path).map_err(RestoreResizeFixtureError::Inspection)?;
    let source_user_sectors = source_inspection
        .user_partition
        .last_lba
        .checked_sub(source_inspection.user_partition.first_lba)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| {
            RestoreResizeFixtureError::Inspection(InspectionError::InvalidGpt(
                "nieprawidłowy zakres USER",
            ))
        })?;
    plan_user_expansion(
        target_metadata.len() / LOGICAL_SECTOR_BYTES,
        source_inspection.user_partition.first_lba,
        source_user_sectors,
    )
    .map_err(RestoreResizeFixtureError::Expansion)?;
    validate_user_fat32(
        source_path,
        source_inspection.raw_nand_offset_bytes,
        &source_inspection.user_partition,
        key,
    )
    .map_err(RestoreResizeFixtureError::UserVerification)?;

    // The existing fixture restore streams and verifies RAWNAND (also for the
    // explicit FULL NAND container) without making an image-sized allocation.
    let mut copy_observer = ContinueRestoreFixture;
    restore_raw_nand_to_fixture(source_path, target_path, &mut copy_observer)
        .map_err(RestoreResizeFixtureError::Restore)?;

    let capacity = target_metadata.len() / LOGICAL_SECTOR_BYTES;
    let mut target = OpenOptions::new()
        .read(true)
        .write(true)
        .open(target_path)
        .map_err(|_| RestoreResizeFixtureError::TargetUnavailable)?;
    let plan = build_fixture_resize_plan(&mut target, capacity, key)?;

    if interrupted_fixture_phase(
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforeClusterRelocation,
    ) {
        return Ok(SyntheticRestoreStatus::InterruptedBefore(
            RestoreAndExpandCheckpoint::BeforeClusterRelocation,
        ));
    }
    for relocation in &plan.relocations {
        let mut cluster = vec![0_u8; plan.cluster_byte_len];
        read_user_plain_from_fixture(
            &mut target,
            plan.user_offset,
            plan.current_user_byte_len,
            key,
            relocation.source_sector * LOGICAL_SECTOR_BYTES,
            &mut cluster,
        )?;
        write_user_plain_to_fixture(
            &mut target,
            plan.user_offset,
            plan.target_user_byte_len,
            key,
            relocation.target_sector * LOGICAL_SECTOR_BYTES,
            &cluster,
        )?;
        let mut verified = vec![0_u8; plan.cluster_byte_len];
        read_user_plain_from_fixture(
            &mut target,
            plan.user_offset,
            plan.target_user_byte_len,
            key,
            relocation.target_sector * LOGICAL_SECTOR_BYTES,
            &mut verified,
        )?;
        if cluster != verified {
            return Err(RestoreResizeFixtureError::VerificationMismatch);
        }
    }
    sync_fixture(&mut target)?;

    if interrupted_fixture_phase(
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforeFatMirrors,
    ) {
        return Ok(SyntheticRestoreStatus::InterruptedBefore(
            RestoreAndExpandCheckpoint::BeforeFatMirrors,
        ));
    }
    write_user_plain_to_fixture(
        &mut target,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        plan.fat_offset,
        &plan.expanded_fat,
    )?;
    write_user_plain_to_fixture(
        &mut target,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        plan.target_mirror_offset,
        &plan.expanded_fat,
    )?;
    verify_fixture_user_plain(&mut target, &plan, key, plan.fat_offset, &plan.expanded_fat)?;
    verify_fixture_user_plain(
        &mut target,
        &plan,
        key,
        plan.target_mirror_offset,
        &plan.expanded_fat,
    )?;
    sync_fixture(&mut target)?;

    if interrupted_fixture_phase(
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforeBackupGpt,
    ) {
        return Ok(SyntheticRestoreStatus::InterruptedBefore(
            RestoreAndExpandCheckpoint::BeforeBackupGpt,
        ));
    }
    write_gpt_table_to_fixture(&mut target, plan.backup_table_lba, &plan.primary_table)?;
    write_sector_to_fixture(&mut target, capacity - 1, &plan.backup_header)?;
    verify_gpt_copy_in_fixture(
        &mut target,
        capacity - 1,
        plan.backup_table_lba,
        &plan.backup_header,
        &plan.primary_table,
    )?;
    sync_fixture(&mut target)?;

    if interrupted_fixture_phase(
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforePrimaryGpt,
    ) {
        return Ok(SyntheticRestoreStatus::InterruptedBefore(
            RestoreAndExpandCheckpoint::BeforePrimaryGpt,
        ));
    }
    write_gpt_table_to_fixture(&mut target, plan.gpt.entries_lba, &plan.primary_table)?;
    write_sector_to_fixture(&mut target, GPT_HEADER_LBA, &plan.primary_header)?;
    verify_gpt_copy_in_fixture(
        &mut target,
        GPT_HEADER_LBA,
        plan.gpt.entries_lba,
        &plan.primary_header,
        &plan.primary_table,
    )?;
    sync_fixture(&mut target)?;

    if interrupted_fixture_phase(
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforeBackupBootSector,
    ) {
        return Ok(SyntheticRestoreStatus::InterruptedBefore(
            RestoreAndExpandCheckpoint::BeforeBackupBootSector,
        ));
    }
    write_user_plain_to_fixture(
        &mut target,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        plan.backup_boot_offset,
        &plan.rewritten_boot,
    )?;
    verify_fixture_user_plain(
        &mut target,
        &plan,
        key,
        plan.backup_boot_offset,
        &plan.rewritten_boot,
    )?;
    sync_fixture(&mut target)?;

    if interrupted_fixture_phase(
        interrupt_before,
        RestoreAndExpandCheckpoint::BeforePrimaryBootSector,
    ) {
        return Ok(SyntheticRestoreStatus::InterruptedBefore(
            RestoreAndExpandCheckpoint::BeforePrimaryBootSector,
        ));
    }
    write_user_plain_to_fixture(
        &mut target,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        0,
        &plan.rewritten_boot,
    )?;
    verify_fixture_user_plain(&mut target, &plan, key, 0, &plan.rewritten_boot)?;
    sync_fixture(&mut target)?;

    drop(target);
    let verified =
        inspect_nand_image(target_path).map_err(RestoreResizeFixtureError::Verification)?;
    validate_user_fat32(target_path, 0, &verified.user_partition, key)
        .map_err(RestoreResizeFixtureError::UserVerification)?;
    Ok(SyntheticRestoreStatus::Completed)
}

/// Inspects a restore+resize fixture without changing it. This parser is kept
/// separate from the transaction planner so a report after a process restart
/// does not depend on in-memory phase state or on the writer's expected plan.
///
/// The report is deliberately fixture-only and opens the path read-only. A
/// `ManualRecoveryFromBackupBoot` result is diagnostic evidence, not an API
/// for writing a backup boot sector back to the file or to a device. The
/// backup-sector probe is intentionally limited to the first 16 KiB encrypted
/// USER data unit, which covers the fixture's FAT32 backup boot sector.
pub fn inspect_restore_resize_fixture_recovery(
    target_path: impl AsRef<Path>,
    key: &BisKey,
) -> Result<FixtureRecoveryReport, FixtureRecoveryError> {
    let target_path = target_path.as_ref();
    let metadata =
        fs::metadata(target_path).map_err(|_| FixtureRecoveryError::TargetUnavailable)?;
    if !metadata.is_file() {
        return Err(FixtureRecoveryError::TargetIsNotRegularFile);
    }
    if metadata.len() % LOGICAL_SECTOR_BYTES != 0 {
        return Err(FixtureRecoveryError::TargetUnsupportedSize);
    }
    let capacity_sectors = metadata.len() / LOGICAL_SECTOR_BYTES;
    if capacity_sectors == 0 {
        return Err(FixtureRecoveryError::TargetUnsupportedSize);
    }
    let mut file = File::open(target_path).map_err(|_| FixtureRecoveryError::TargetUnavailable)?;
    inspect_recovery_reader(&mut file, capacity_sectors, key)
}

fn inspect_recovery_reader(
    file: &mut File,
    capacity_sectors: u64,
    key: &BisKey,
) -> Result<FixtureRecoveryReport, FixtureRecoveryError> {
    let primary_gpt_data = recover_gpt_copy(file, GPT_HEADER_LBA, capacity_sectors);
    let backup_gpt_data = recover_gpt_copy(file, capacity_sectors - 1, capacity_sectors);
    let primary_gpt = recovery_gpt_copy(&primary_gpt_data);
    let backup_gpt = recovery_gpt_copy(&backup_gpt_data);

    let mut user_starts = Vec::new();
    for gpt in [primary_gpt_data.as_ref(), backup_gpt_data.as_ref()]
        .into_iter()
        .flatten()
    {
        if !user_starts.contains(&gpt.user_first_lba) {
            user_starts.push(gpt.user_first_lba);
        }
    }
    let primary_boot_data = user_starts
        .iter()
        .find_map(|first_lba| recover_boot_at(file, *first_lba, capacity_sectors, key));
    let backup_boot_data = user_starts.iter().find_map(|first_lba| {
        recover_backup_boot_in_first_unit(file, *first_lba, capacity_sectors, key)
    });
    let primary_boot = recovery_boot_copy(&primary_boot_data);
    let backup_boot = recovery_boot_copy(&backup_boot_data);

    let gpts_match = matches!(
        (&primary_gpt_data, &backup_gpt_data),
        (Some(primary), Some(backup))
            if primary.user_first_lba == backup.user_first_lba
                && primary.user_last_lba == backup.user_last_lba
    );
    let boots_match = matches!(
        (&primary_boot_data, &backup_boot_data),
        (Some(primary), Some(backup))
            if primary.user_first_lba == backup.user_first_lba
                && primary.user_sectors == backup.user_sectors
                && primary.bytes == backup.bytes
                && primary.fat_mirrors_match
                && backup.fat_mirrors_match
    );
    let primary_matches_gpt = matches!(
        (&primary_boot_data, &primary_gpt_data),
        (Some(boot), Some(gpt))
            if boot.user_first_lba == gpt.user_first_lba
                && boot.user_sectors == gpt.user_last_lba - gpt.user_first_lba + 1
    );
    let disposition = if gpts_match && boots_match && primary_matches_gpt {
        FixtureRecoveryDisposition::Committed
    } else if primary_boot_data.is_some() {
        // A parseable primary boot sector remains the active FAT geometry even
        // where a prior phase has already changed a GPT copy or FAT mirror.
        FixtureRecoveryDisposition::PreviousGeometryActive
    } else if backup_boot_data.is_some() {
        FixtureRecoveryDisposition::ManualRecoveryFromBackupBoot
    } else {
        FixtureRecoveryDisposition::Inconclusive
    };

    Ok(FixtureRecoveryReport {
        capacity_sectors,
        primary_gpt,
        backup_gpt,
        primary_boot,
        backup_boot,
        disposition,
    })
}

#[derive(Clone)]
struct RecoveredGpt {
    user_first_lba: u64,
    user_last_lba: u64,
}

#[derive(Clone)]
struct RecoveredBoot {
    bytes: [u8; LOGICAL_SECTOR_BYTES as usize],
    user_first_lba: u64,
    user_sectors: u64,
    fat_mirrors_match: bool,
}

fn recovery_gpt_copy(gpt: &Option<RecoveredGpt>) -> FixtureRecoveryGptCopy {
    match gpt {
        Some(gpt) => FixtureRecoveryGptCopy::Valid {
            user_first_lba: gpt.user_first_lba,
            user_last_lba: gpt.user_last_lba,
        },
        None => FixtureRecoveryGptCopy::Invalid,
    }
}

fn recovery_boot_copy(boot: &Option<RecoveredBoot>) -> FixtureRecoveryBootCopy {
    match boot {
        Some(boot) => FixtureRecoveryBootCopy::Valid {
            user_first_lba: boot.user_first_lba,
            user_sectors: boot.user_sectors,
            fat_mirrors_match: boot.fat_mirrors_match,
        },
        None => FixtureRecoveryBootCopy::Invalid,
    }
}

fn recover_gpt_copy(
    file: &mut File,
    header_lba: u64,
    capacity_sectors: u64,
) -> Option<RecoveredGpt> {
    let mut header = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    recovery_read_at(
        file,
        header_lba.checked_mul(LOGICAL_SECTOR_BYTES)?,
        &mut header,
    )?;
    if &header[..8] != b"EFI PART" {
        return None;
    }
    let header_size = read_u32(&header, 12).ok()? as usize;
    if !(GPT_MIN_HEADER_SIZE..=header.len()).contains(&header_size)
        || read_u32(&header, 16).ok()? != crc32_with_zeroed_field(&header[..header_size], 16, 4)
        || read_u64(&header, 24).ok()? != header_lba
    {
        return None;
    }
    let entries_lba = read_u64(&header, 72).ok()?;
    let entry_count = read_u32(&header, 80).ok()? as usize;
    let entry_size = read_u32(&header, 84).ok()? as usize;
    if entry_count == 0 || entry_size < GPT_ENTRY_MIN_SIZE {
        return None;
    }
    let table_len = entry_count.checked_mul(entry_size)?;
    if table_len > MAX_GPT_TABLE_BYTES {
        return None;
    }
    let table_offset = entries_lba.checked_mul(LOGICAL_SECTOR_BYTES)?;
    let table_end = table_offset.checked_add(table_len as u64)?;
    if table_end > capacity_sectors.checked_mul(LOGICAL_SECTOR_BYTES)? {
        return None;
    }
    let mut table = vec![0_u8; table_len];
    recovery_read_at(file, table_offset, &mut table)?;
    if crc32fast::hash(&table) != read_u32(&header, 88).ok()? {
        return None;
    }
    let mut user = None;
    let mut highest_last_lba = 0_u64;
    for entry in table.chunks_exact(entry_size) {
        if entry[..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let first_lba = read_u64(entry, 32).ok()?;
        let last_lba = read_u64(entry, 40).ok()?;
        if first_lba > last_lba || last_lba >= capacity_sectors {
            return None;
        }
        highest_last_lba = highest_last_lba.max(last_lba);
        if decode_gpt_name(&entry[56..GPT_ENTRY_MIN_SIZE]) == "USER" {
            user = Some((first_lba, last_lba));
        }
    }
    let (user_first_lba, user_last_lba) = user?;
    (user_last_lba == highest_last_lba).then_some(RecoveredGpt {
        user_first_lba,
        user_last_lba,
    })
}

fn recover_boot_at(
    file: &mut File,
    user_first_lba: u64,
    capacity_sectors: u64,
    key: &BisKey,
) -> Option<RecoveredBoot> {
    let max_user_sectors = recovery_max_user_sectors(user_first_lba, capacity_sectors)?;
    let mut bytes = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    read_user_plain_from_fixture(
        file,
        user_first_lba.checked_mul(LOGICAL_SECTOR_BYTES)?,
        max_user_sectors.checked_mul(LOGICAL_SECTOR_BYTES)?,
        key,
        0,
        &mut bytes,
    )
    .ok()?;
    recover_boot_from_bytes(file, user_first_lba, max_user_sectors, key, bytes)
}

fn recover_backup_boot_in_first_unit(
    file: &mut File,
    user_first_lba: u64,
    capacity_sectors: u64,
    key: &BisKey,
) -> Option<RecoveredBoot> {
    let max_user_sectors = recovery_max_user_sectors(user_first_lba, capacity_sectors)?;
    let first_unit_fits = max_user_sectors
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .is_some_and(|bytes| bytes >= BIS_DATA_UNIT_BYTES as u64);
    if !first_unit_fits {
        return None;
    }
    let mut first_unit = [0_u8; BIS_DATA_UNIT_BYTES];
    read_user_plain_from_fixture(
        file,
        user_first_lba.checked_mul(LOGICAL_SECTOR_BYTES)?,
        max_user_sectors.checked_mul(LOGICAL_SECTOR_BYTES)?,
        key,
        0,
        &mut first_unit,
    )
    .ok()?;
    for sector in 1..(BIS_DATA_UNIT_BYTES / LOGICAL_SECTOR_BYTES as usize) {
        let mut bytes = [0_u8; LOGICAL_SECTOR_BYTES as usize];
        let start = sector * LOGICAL_SECTOR_BYTES as usize;
        bytes.copy_from_slice(&first_unit[start..start + LOGICAL_SECTOR_BYTES as usize]);
        if let Some(boot) =
            recover_boot_from_bytes(file, user_first_lba, max_user_sectors, key, bytes)
        {
            if boot.bytes[50..52] == (sector as u16).to_le_bytes() {
                return Some(boot);
            }
        }
    }
    None
}

fn recover_boot_from_bytes(
    file: &mut File,
    user_first_lba: u64,
    max_user_sectors: u64,
    key: &BisKey,
    bytes: [u8; LOGICAL_SECTOR_BYTES as usize],
) -> Option<RecoveredBoot> {
    let total_16 = u16::from_le_bytes([bytes[19], bytes[20]]);
    let user_sectors = if total_16 != 0 {
        u64::from(total_16)
    } else {
        u64::from(u32::from_le_bytes([
            bytes[32], bytes[33], bytes[34], bytes[35],
        ]))
    };
    if user_sectors == 0 || user_sectors > max_user_sectors {
        return None;
    }
    let boot = parse_fat32_boot_sector(&bytes, user_sectors).ok()?;
    let user_offset = user_first_lba.checked_mul(LOGICAL_SECTOR_BYTES)?;
    let user_byte_len = user_sectors.checked_mul(LOGICAL_SECTOR_BYTES)?;
    let fat_len =
        usize::try_from(u64::from(boot.sectors_per_fat).checked_mul(LOGICAL_SECTOR_BYTES)?).ok()?;
    let fat_offset = u64::from(boot.reserved_sectors).checked_mul(LOGICAL_SECTOR_BYTES)?;
    let mirror_offset = fat_offset.checked_add(fat_len as u64)?;
    let mut primary_fat = vec![0_u8; fat_len];
    let mut mirror_fat = vec![0_u8; fat_len];
    let fat_mirrors_match = read_user_plain_from_fixture(
        file,
        user_offset,
        user_byte_len,
        key,
        fat_offset,
        &mut primary_fat,
    )
    .and_then(|_| {
        read_user_plain_from_fixture(
            file,
            user_offset,
            user_byte_len,
            key,
            mirror_offset,
            &mut mirror_fat,
        )
    })
    .is_ok_and(|_| primary_fat == mirror_fat);
    Some(RecoveredBoot {
        bytes,
        user_first_lba,
        user_sectors,
        fat_mirrors_match,
    })
}

fn recovery_max_user_sectors(user_first_lba: u64, capacity_sectors: u64) -> Option<u64> {
    capacity_sectors
        .checked_sub(user_first_lba)?
        .checked_sub(BACKUP_GPT_SECTORS)
}

fn recovery_read_at(file: &mut File, offset: u64, output: &mut [u8]) -> Option<()> {
    read_sector_aligned_at(file, offset, output).ok()
}

struct FixtureResizePlan {
    gpt: ResizeGpt,
    expansion: UserExpansionPlan,
    fat32: Fat32Inspection,
    fat32_expansion: Fat32ExpansionPlan,
    user_offset: u64,
    current_user_byte_len: u64,
    target_user_byte_len: u64,
    cluster_byte_len: usize,
    relocations: Vec<Fat32ClusterRelocation>,
    fat_offset: u64,
    target_mirror_offset: u64,
    expanded_fat: Vec<u8>,
    primary_header: [u8; LOGICAL_SECTOR_BYTES as usize],
    primary_table: Vec<u8>,
    backup_header: [u8; LOGICAL_SECTOR_BYTES as usize],
    backup_table_lba: u64,
    rewritten_boot: [u8; LOGICAL_SECTOR_BYTES as usize],
    backup_boot_offset: u64,
}

#[cfg(any(target_os = "windows", test))]
fn build_resize_plan_from_source(
    source: &ImageReader,
    raw_nand_offset: u64,
    target_capacity_sectors: u64,
    key: &BisKey,
) -> Result<FixtureResizePlan, RestoreResizeFixtureError> {
    build_resize_plan_with_reader(
        &mut |offset, output| {
            let at = raw_nand_offset
                .checked_add(offset)
                .ok_or(RestoreResizeFixtureError::SourceUnavailable)?;
            source
                .read_exact_at(at, output)
                .map_err(RestoreResizeFixtureError::Inspection)
        },
        target_capacity_sectors,
        key,
    )
}

fn build_fixture_resize_plan(
    target: &mut File,
    target_capacity_sectors: u64,
    key: &BisKey,
) -> Result<FixtureResizePlan, RestoreResizeFixtureError> {
    build_resize_plan_with_reader(
        &mut |offset, output| read_fixture_at(target, offset, output),
        target_capacity_sectors,
        key,
    )
}

fn build_resize_plan_with_reader(
    read_at: &mut impl FnMut(u64, &mut [u8]) -> Result<(), RestoreResizeFixtureError>,
    target_capacity_sectors: u64,
    key: &BisKey,
) -> Result<FixtureResizePlan, RestoreResizeFixtureError> {
    let gpt = parse_resize_gpt_with_reader(read_at)?;
    let current_user_sectors = gpt
        .user_last_lba
        .checked_sub(gpt.user_first_lba)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| {
            RestoreResizeFixtureError::Model(SyntheticRestoreError::Inspection(
                InspectionError::InvalidGpt("nieprawidłowy zakres USER"),
            ))
        })?;
    let expansion = plan_user_expansion(
        target_capacity_sectors,
        gpt.user_first_lba,
        current_user_sectors,
    )
    .map_err(RestoreResizeFixtureError::Expansion)?;
    let user_offset = gpt
        .user_first_lba
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(RestoreResizeFixtureError::WriteFailed)?;
    let current_user_byte_len = current_user_sectors
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(RestoreResizeFixtureError::WriteFailed)?;
    let target_user_byte_len = expansion
        .target_user_sectors
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(RestoreResizeFixtureError::WriteFailed)?;

    let mut boot_sector = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    read_user_plain_with_reader(
        read_at,
        user_offset,
        current_user_byte_len,
        key,
        0,
        &mut boot_sector,
    )?;
    let boot = parse_fat32_boot_sector(&boot_sector, current_user_sectors)
        .map_err(RestoreResizeFixtureError::UserFilesystem)?;
    let backup_boot_offset = u64::from(boot.backup_boot_sector) * LOGICAL_SECTOR_BYTES;
    let mut backup_boot_sector = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    read_user_plain_with_reader(
        read_at,
        user_offset,
        current_user_byte_len,
        key,
        backup_boot_offset,
        &mut backup_boot_sector,
    )?;
    if boot_sector != backup_boot_sector {
        return Err(RestoreResizeFixtureError::UserFilesystem(
            UserFilesystemError::InvalidBootSector("zapasowy boot sector FAT32 różni się"),
        ));
    }
    let fat_len = usize::try_from(u64::from(boot.sectors_per_fat) * LOGICAL_SECTOR_BYTES)
        .map_err(|_| RestoreResizeFixtureError::WriteFailed)?;
    let fat_offset = u64::from(boot.reserved_sectors) * LOGICAL_SECTOR_BYTES;
    let mut fat = vec![0_u8; fat_len];
    let mut mirror = vec![0_u8; fat_len];
    read_user_plain_with_reader(
        read_at,
        user_offset,
        current_user_byte_len,
        key,
        fat_offset,
        &mut fat,
    )?;
    read_user_plain_with_reader(
        read_at,
        user_offset,
        current_user_byte_len,
        key,
        fat_offset + fat_len as u64,
        &mut mirror,
    )?;
    if fat != mirror {
        return Err(RestoreResizeFixtureError::UserFilesystem(
            UserFilesystemError::InvalidBootSector("kopie FAT różnią się"),
        ));
    }
    let fat32 = inspect_fat32(&boot, &fat).map_err(RestoreResizeFixtureError::UserFilesystem)?;
    let fat_expansion = plan_fat32_expansion(&boot, expansion.target_user_sectors)
        .map_err(RestoreResizeFixtureError::Fat32Plan)?;
    let relocations = plan_fat32_cluster_relocations(&boot, &fat, &fat_expansion)
        .map_err(RestoreResizeFixtureError::UserFilesystem)?;
    let expanded_fat = build_expanded_fat(&fat, &fat_expansion)
        .map_err(RestoreResizeFixtureError::UserFilesystem)?;
    let target_mirror_offset = fat_offset
        .checked_add(expanded_fat.len() as u64)
        .ok_or(RestoreResizeFixtureError::WriteFailed)?;
    let target_user_last_lba = gpt
        .user_first_lba
        .checked_add(expansion.target_user_sectors)
        .and_then(|value| value.checked_sub(1))
        .ok_or(RestoreResizeFixtureError::WriteFailed)?;
    let (primary_header, primary_table, backup_header, backup_table_lba) =
        expanded_gpt_metadata(&gpt, target_capacity_sectors, target_user_last_lba)
            .map_err(RestoreResizeFixtureError::Model)?;
    let rewritten_boot = rewrite_fat32_boot_sector(&boot_sector, &fat_expansion)
        .map_err(RestoreResizeFixtureError::UserFilesystem)?;

    Ok(FixtureResizePlan {
        gpt,
        expansion,
        fat32,
        fat32_expansion: fat_expansion,
        user_offset,
        current_user_byte_len,
        target_user_byte_len,
        cluster_byte_len: usize::try_from(
            u64::from(boot.sectors_per_cluster) * LOGICAL_SECTOR_BYTES,
        )
        .map_err(|_| RestoreResizeFixtureError::WriteFailed)?,
        relocations,
        fat_offset,
        target_mirror_offset,
        expanded_fat,
        primary_header,
        primary_table,
        backup_header,
        backup_table_lba,
        rewritten_boot,
        backup_boot_offset,
    })
}

#[cfg(all(target_os = "windows", test))]
fn parse_resize_gpt_from_fixture(
    target: &mut File,
) -> Result<ResizeGpt, RestoreResizeFixtureError> {
    parse_resize_gpt_with_reader(&mut |offset, output| read_fixture_at(target, offset, output))
}

fn parse_resize_gpt_with_reader(
    read_at: &mut impl FnMut(u64, &mut [u8]) -> Result<(), RestoreResizeFixtureError>,
) -> Result<ResizeGpt, RestoreResizeFixtureError> {
    let mut header = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    read_at(GPT_HEADER_LBA * LOGICAL_SECTOR_BYTES, &mut header)?;
    if &header[..8] != b"EFI PART" {
        return Err(RestoreResizeFixtureError::Inspection(
            InspectionError::InvalidGpt("brak sygnatury EFI PART"),
        ));
    }
    let header_size =
        read_u32(&header, 12).map_err(RestoreResizeFixtureError::Inspection)? as usize;
    if !(GPT_MIN_HEADER_SIZE..=header.len()).contains(&header_size)
        || read_u32(&header, 16).map_err(RestoreResizeFixtureError::Inspection)?
            != crc32_with_zeroed_field(&header[..header_size], 16, 4)
        || read_u64(&header, 24).map_err(RestoreResizeFixtureError::Inspection)? != GPT_HEADER_LBA
    {
        return Err(RestoreResizeFixtureError::Inspection(
            InspectionError::InvalidGpt("niepoprawny primary GPT"),
        ));
    }
    let entries_lba = read_u64(&header, 72).map_err(RestoreResizeFixtureError::Inspection)?;
    let entry_count =
        read_u32(&header, 80).map_err(RestoreResizeFixtureError::Inspection)? as usize;
    let entry_size = read_u32(&header, 84).map_err(RestoreResizeFixtureError::Inspection)? as usize;
    if entry_count == 0 || entry_size < GPT_ENTRY_MIN_SIZE {
        return Err(RestoreResizeFixtureError::Inspection(
            InspectionError::InvalidGpt("nieprawidłowy opis tablicy partycji"),
        ));
    }
    let table_len = entry_count
        .checked_mul(entry_size)
        .filter(|length| *length <= MAX_GPT_TABLE_BYTES)
        .ok_or_else(|| {
            RestoreResizeFixtureError::Inspection(InspectionError::InvalidGpt(
                "tablica partycji jest zbyt duża",
            ))
        })?;
    let table_offset = entries_lba
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or_else(|| {
            RestoreResizeFixtureError::Inspection(InspectionError::InvalidGpt(
                "przepełnienie offsetu tablicy",
            ))
        })?;
    let mut table = vec![0_u8; table_len];
    read_at(table_offset, &mut table)?;
    if crc32fast::hash(&table)
        != read_u32(&header, 88).map_err(RestoreResizeFixtureError::Inspection)?
    {
        return Err(RestoreResizeFixtureError::Inspection(
            InspectionError::InvalidGpt("niepoprawne CRC tablicy partycji"),
        ));
    }
    let mut user = None;
    let mut highest_last_lba = 0_u64;
    for (index, entry) in table.chunks_exact(entry_size).enumerate() {
        if entry[..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let first_lba = read_u64(entry, 32).map_err(RestoreResizeFixtureError::Inspection)?;
        let last_lba = read_u64(entry, 40).map_err(RestoreResizeFixtureError::Inspection)?;
        if first_lba > last_lba {
            return Err(RestoreResizeFixtureError::Inspection(
                InspectionError::InvalidGpt("zakres partycji jest nieprawidłowy"),
            ));
        }
        highest_last_lba = highest_last_lba.max(last_lba);
        if decode_gpt_name(&entry[56..GPT_ENTRY_MIN_SIZE]) == "USER" {
            user = Some((index * entry_size, first_lba, last_lba));
        }
    }
    let (user_entry_offset, user_first_lba, user_last_lba) = user.ok_or_else(|| {
        RestoreResizeFixtureError::Inspection(InspectionError::MissingUserPartition)
    })?;
    if user_last_lba != highest_last_lba {
        return Err(RestoreResizeFixtureError::Inspection(
            InspectionError::InvalidGpt("USER nie jest ostatnią partycją"),
        ));
    }
    Ok(ResizeGpt {
        header,
        header_size,
        table,
        entries_lba,
        user_entry_offset,
        user_first_lba,
        user_last_lba,
    })
}

fn interrupted_fixture_phase(
    requested: Option<RestoreAndExpandCheckpoint>,
    checkpoint: RestoreAndExpandCheckpoint,
) -> bool {
    requested == Some(checkpoint)
}

fn read_fixture_at(
    file: &mut File,
    offset: u64,
    output: &mut [u8],
) -> Result<(), RestoreResizeFixtureError> {
    let length = output.len();
    let failure = |step: &'static str, error: io::Error| RestoreResizeFixtureError::ReadFailed {
        offset,
        length,
        step,
        os_error: error.raw_os_error(),
        reason: error.to_string(),
    };
    read_sector_aligned_at(file, offset, output).map_err(|(step, error)| failure(step, error))
}

// Raw Windows disk handles require whole-sector reads. GPT tables can occupy
// 1408 bytes of a 1536-byte (three-sector) area, so read the enclosing sectors.
fn read_sector_aligned_at(
    file: &mut File,
    offset: u64,
    output: &mut [u8],
) -> Result<(), (&'static str, io::Error)> {
    if output.is_empty() {
        return Ok(());
    }
    let sector = LOGICAL_SECTOR_BYTES as usize;
    let prefix = (offset % LOGICAL_SECTOR_BYTES) as usize;
    let physical_offset = offset - prefix as u64;
    let covered = prefix
        .checked_add(output.len())
        .ok_or_else(|| ("range", io::Error::from(io::ErrorKind::InvalidInput)))?;
    let physical_length = covered
        .checked_add(sector - 1)
        .map(|length| length / sector * sector)
        .ok_or_else(|| ("range", io::Error::from(io::ErrorKind::InvalidInput)))?;
    physical_offset
        .checked_add(physical_length as u64)
        .ok_or_else(|| ("range", io::Error::from(io::ErrorKind::InvalidInput)))?;
    file.seek(SeekFrom::Start(physical_offset))
        .map_err(|error| ("seek", error))?;
    if prefix == 0 && physical_length == output.len() {
        return file.read_exact(output).map_err(|error| ("read", error));
    }
    let mut sectors = vec![0_u8; physical_length];
    file.read_exact(&mut sectors)
        .map_err(|error| ("read", error))?;
    output.copy_from_slice(&sectors[prefix..prefix + output.len()]);
    Ok(())
}

fn write_fixture_at(
    file: &mut File,
    offset: u64,
    input: &[u8],
) -> Result<(), RestoreResizeFixtureError> {
    #[cfg(test)]
    record_fixture_volatile_write(file, offset, input.len())?;
    #[cfg(test)]
    if let Some(partial_len) = injected_fixture_write_failure(input.len()) {
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(&input[..partial_len]))
            .map_err(|_| RestoreResizeFixtureError::WriteFailed)?;
        return Err(RestoreResizeFixtureError::WriteFailed);
    }
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.write_all(input))
        .map_err(|_| RestoreResizeFixtureError::WriteFailed)
}

fn sync_fixture(file: &mut File) -> Result<(), RestoreResizeFixtureError> {
    #[cfg(test)]
    match injected_fixture_sync_fault() {
        FixtureSyncFault::Fail => return Err(RestoreResizeFixtureError::WriteFailed),
        FixtureSyncFault::Restart => {
            discard_fixture_volatile_writes(file)?;
            return Err(RestoreResizeFixtureError::Restarted);
        }
        FixtureSyncFault::None => {}
    }
    file.sync_all()
        .map_err(|_| RestoreResizeFixtureError::WriteFailed)?;
    #[cfg(test)]
    clear_fixture_volatile_writes();
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum FixtureFault {
    PartialWrite { operation: usize, byte_len: usize },
    Sync { operation: usize },
    RestartBeforeSync { operation: usize },
}

#[cfg(test)]
#[derive(Default)]
struct FixtureFaultState {
    fault: Option<FixtureFault>,
    writes: usize,
    syncs: usize,
    volatile_writes: Vec<FixtureVolatileWrite>,
}

#[cfg(test)]
#[derive(Clone)]
struct FixtureVolatileWrite {
    offset: u64,
    original: Vec<u8>,
}

#[cfg(test)]
thread_local! {
    static FIXTURE_FAULT_STATE: RefCell<FixtureFaultState> = RefCell::new(FixtureFaultState::default());
}

#[cfg(test)]
fn injected_fixture_write_failure(input_len: usize) -> Option<usize> {
    FIXTURE_FAULT_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.writes += 1;
        match state.fault {
            Some(FixtureFault::PartialWrite {
                operation,
                byte_len,
            }) if state.writes == operation => Some(byte_len.min(input_len)),
            _ => None,
        }
    })
}

#[cfg(test)]
enum FixtureSyncFault {
    None,
    Fail,
    Restart,
}

#[cfg(test)]
fn injected_fixture_sync_fault() -> FixtureSyncFault {
    FIXTURE_FAULT_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.syncs += 1;
        match state.fault {
            Some(FixtureFault::Sync { operation }) if state.syncs == operation => {
                FixtureSyncFault::Fail
            }
            Some(FixtureFault::RestartBeforeSync { operation }) if state.syncs == operation => {
                FixtureSyncFault::Restart
            }
            _ => FixtureSyncFault::None,
        }
    })
}

#[cfg(test)]
fn record_fixture_volatile_write(
    file: &mut File,
    offset: u64,
    len: usize,
) -> Result<(), RestoreResizeFixtureError> {
    let mut original = vec![0_u8; len];
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(&mut original))
        .map_err(|_| RestoreResizeFixtureError::WriteFailed)?;
    FIXTURE_FAULT_STATE.with(|state| {
        state
            .borrow_mut()
            .volatile_writes
            .push(FixtureVolatileWrite { offset, original });
    });
    Ok(())
}

#[cfg(test)]
fn discard_fixture_volatile_writes(file: &mut File) -> Result<(), RestoreResizeFixtureError> {
    let writes = FIXTURE_FAULT_STATE.with(|state| {
        let mut state = state.borrow_mut();
        std::mem::take(&mut state.volatile_writes)
    });
    for write in writes.iter().rev() {
        file.seek(SeekFrom::Start(write.offset))
            .and_then(|_| file.write_all(&write.original))
            .map_err(|_| RestoreResizeFixtureError::WriteFailed)?;
    }
    file.sync_all()
        .map_err(|_| RestoreResizeFixtureError::WriteFailed)
}

#[cfg(test)]
fn clear_fixture_volatile_writes() {
    FIXTURE_FAULT_STATE.with(|state| state.borrow_mut().volatile_writes.clear());
}

#[cfg(test)]
fn with_fixture_fault<T>(fault: FixtureFault, action: impl FnOnce() -> T) -> T {
    FIXTURE_FAULT_STATE.with(|state| {
        *state.borrow_mut() = FixtureFaultState {
            fault: Some(fault),
            writes: 0,
            syncs: 0,
            volatile_writes: Vec::new(),
        };
    });
    let result = action();
    FIXTURE_FAULT_STATE.with(|state| *state.borrow_mut() = FixtureFaultState::default());
    result
}

fn read_user_plain_from_fixture(
    file: &mut File,
    user_offset: u64,
    user_byte_len: u64,
    key: &BisKey,
    offset: u64,
    output: &mut [u8],
) -> Result<(), RestoreResizeFixtureError> {
    read_user_plain_with_reader(
        &mut |at, buffer| read_fixture_at(file, at, buffer),
        user_offset,
        user_byte_len,
        key,
        offset,
        output,
    )
}

fn read_user_plain_with_reader(
    read_at: &mut impl FnMut(u64, &mut [u8]) -> Result<(), RestoreResizeFixtureError>,
    user_offset: u64,
    user_byte_len: u64,
    key: &BisKey,
    offset: u64,
    output: &mut [u8],
) -> Result<(), RestoreResizeFixtureError> {
    let end = offset
        .checked_add(output.len() as u64)
        .ok_or(RestoreResizeFixtureError::WriteFailed)?;
    if end > user_byte_len {
        return Err(RestoreResizeFixtureError::WriteFailed);
    }
    let crypto = SwitchAesXts128::new(key);
    let mut cursor = offset;
    let mut written = 0;
    while written < output.len() {
        let unit_index = cursor / BIS_DATA_UNIT_BYTES as u64;
        let within = (cursor % BIS_DATA_UNIT_BYTES as u64) as usize;
        let wanted = (output.len() - written).min(BIS_DATA_UNIT_BYTES - within);
        let unit_offset = user_offset
            .checked_add(unit_index * BIS_DATA_UNIT_BYTES as u64)
            .ok_or(RestoreResizeFixtureError::WriteFailed)?;
        let mut unit = vec![0_u8; BIS_DATA_UNIT_BYTES];
        read_at(unit_offset, &mut unit)?;
        crypto
            .decrypt_unit(unit_index, &mut unit)
            .expect("fixed 16 KiB input");
        output[written..written + wanted].copy_from_slice(&unit[within..within + wanted]);
        cursor += wanted as u64;
        written += wanted;
    }
    Ok(())
}

fn write_user_plain_to_fixture(
    file: &mut File,
    user_offset: u64,
    user_byte_len: u64,
    key: &BisKey,
    offset: u64,
    input: &[u8],
) -> Result<(), RestoreResizeFixtureError> {
    let end = offset
        .checked_add(input.len() as u64)
        .ok_or(RestoreResizeFixtureError::WriteFailed)?;
    if end > user_byte_len {
        return Err(RestoreResizeFixtureError::WriteFailed);
    }
    let crypto = SwitchAesXts128::new(key);
    let mut cursor = offset;
    let mut consumed = 0;
    while consumed < input.len() {
        let unit_index = cursor / BIS_DATA_UNIT_BYTES as u64;
        let within = (cursor % BIS_DATA_UNIT_BYTES as u64) as usize;
        let wanted = (input.len() - consumed).min(BIS_DATA_UNIT_BYTES - within);
        let unit_offset = user_offset
            .checked_add(unit_index * BIS_DATA_UNIT_BYTES as u64)
            .ok_or(RestoreResizeFixtureError::WriteFailed)?;
        let mut unit = vec![0_u8; BIS_DATA_UNIT_BYTES];
        read_fixture_at(file, unit_offset, &mut unit)?;
        crypto
            .decrypt_unit(unit_index, &mut unit)
            .expect("fixed 16 KiB input");
        unit[within..within + wanted].copy_from_slice(&input[consumed..consumed + wanted]);
        crypto
            .encrypt_unit(unit_index, &mut unit)
            .expect("fixed 16 KiB input");
        write_fixture_at(file, unit_offset, &unit)?;
        cursor += wanted as u64;
        consumed += wanted;
    }
    Ok(())
}

fn verify_fixture_user_plain(
    file: &mut File,
    plan: &FixtureResizePlan,
    key: &BisKey,
    offset: u64,
    expected: &[u8],
) -> Result<(), RestoreResizeFixtureError> {
    let mut actual = vec![0_u8; expected.len()];
    read_user_plain_from_fixture(
        file,
        plan.user_offset,
        plan.target_user_byte_len,
        key,
        offset,
        &mut actual,
    )?;
    if actual != expected {
        return Err(RestoreResizeFixtureError::VerificationMismatch);
    }
    Ok(())
}

fn write_gpt_table_to_fixture(
    file: &mut File,
    lba: u64,
    table: &[u8],
) -> Result<(), RestoreResizeFixtureError> {
    let rounded_len =
        table.len().div_ceil(LOGICAL_SECTOR_BYTES as usize) * LOGICAL_SECTOR_BYTES as usize;
    let mut padded = vec![0_u8; rounded_len];
    padded[..table.len()].copy_from_slice(table);
    write_fixture_at(file, lba * LOGICAL_SECTOR_BYTES, &padded)
}

fn write_sector_to_fixture(
    file: &mut File,
    lba: u64,
    sector: &[u8; LOGICAL_SECTOR_BYTES as usize],
) -> Result<(), RestoreResizeFixtureError> {
    write_fixture_at(file, lba * LOGICAL_SECTOR_BYTES, sector)
}

fn verify_gpt_copy_in_fixture(
    file: &mut File,
    header_lba: u64,
    table_lba: u64,
    expected_header: &[u8; LOGICAL_SECTOR_BYTES as usize],
    expected_table: &[u8],
) -> Result<(), RestoreResizeFixtureError> {
    let mut header = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    read_fixture_at(file, header_lba * LOGICAL_SECTOR_BYTES, &mut header)?;
    if header != *expected_header {
        return Err(RestoreResizeFixtureError::VerificationMismatch);
    }
    let mut table = vec![0_u8; expected_table.len()];
    read_fixture_at(file, table_lba * LOGICAL_SECTOR_BYTES, &mut table)?;
    if table != expected_table {
        return Err(RestoreResizeFixtureError::VerificationMismatch);
    }
    Ok(())
}

#[derive(Debug)]
pub enum RestoreResizeFixtureError {
    Inspection(InspectionError),
    SourceUnavailable,
    SourceMatchesTarget,
    TargetUnavailable,
    TargetIsNotRegularFile,
    SourceUnsupportedSize,
    TargetUnsupportedSize,
    Restore(RestoreFixtureError),
    Model(SyntheticRestoreError),
    UserFilesystem(UserFilesystemError),
    Expansion(PlanError),
    Fat32Plan(Fat32PlanError),
    WriteFailed,
    ReadFailed {
        offset: u64,
        length: usize,
        step: &'static str,
        os_error: Option<i32>,
        reason: String,
    },
    Restarted,
    VerificationMismatch,
    Verification(InspectionError),
    // Kept separate from GPT inspection so a wrong key or malformed FAT is
    // never surfaced as a raw I/O detail by an adapter.
    UserVerification(UserFilesystemError),
}

impl fmt::Display for RestoreResizeFixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection(error) | Self::Verification(error) => error.fmt(formatter),
            Self::SourceUnavailable => {
                write!(formatter, "Nie można odczytać pliku fixture źródła.")
            }
            Self::SourceMatchesTarget => write!(
                formatter,
                "Źródło i plik fixture celu są tym samym plikiem."
            ),
            Self::TargetUnavailable => write!(formatter, "Nie można otworzyć pliku fixture celu."),
            Self::TargetIsNotRegularFile => {
                write!(formatter, "Silnik fixture wymaga zwykłego pliku celu.")
            }
            Self::SourceUnsupportedSize => {
                write!(formatter, "Źródło fixture przekracza obsługiwany rozmiar.")
            }
            Self::TargetUnsupportedSize => {
                write!(
                    formatter,
                    "Plik fixture celu musi kończyć się na granicy sektora."
                )
            }
            Self::Restore(_)
            | Self::Model(_)
            | Self::UserFilesystem(_)
            | Self::Expansion(_)
            | Self::Fat32Plan(_) => write!(
                formatter,
                "Nie można bezpiecznie wykonać modelu restore+resize."
            ),
            Self::WriteFailed => write!(formatter, "Nie udało się zapisać pliku fixture celu."),
            Self::ReadFailed { .. } => write!(formatter, "Nie udało się odczytać danych celu."),
            Self::Restarted => write!(
                formatter,
                "Zasymulowano restart przed synchronizacją pliku fixture."
            ),
            Self::VerificationMismatch => write!(
                formatter,
                "Weryfikacja zapisanej fazy restore+resize nie powiodła się."
            ),
            Self::UserVerification(_) => {
                write!(formatter, "Końcowa weryfikacja USER nie powiodła się.")
            }
        }
    }
}

fn interrupted_image(
    restored: &[u8],
    requested: Option<RestoreAndExpandCheckpoint>,
    checkpoint: RestoreAndExpandCheckpoint,
) -> Option<SyntheticRestoreOutcome> {
    (requested == Some(checkpoint)).then(|| SyntheticRestoreOutcome {
        raw_nand: restored.to_vec(),
        status: SyntheticRestoreStatus::InterruptedBefore(checkpoint),
    })
}

fn raw_nand_from_image_bytes(source: &[u8]) -> Result<&[u8], InspectionError> {
    let sector_len = LOGICAL_SECTOR_BYTES as usize;
    if source.len() < 2 * sector_len {
        return Err(InspectionError::ImageTooSmall);
    }
    if source.get(sector_len..sector_len + 8) == Some(b"EFI PART".as_slice()) {
        return Ok(source);
    }
    let raw_offset = usize::try_from(FULL_NAND_BOOT_AREA_BYTES).expect("8 MiB fits usize");
    let header_offset = raw_offset + sector_len;
    if source.get(header_offset..header_offset + 8) != Some(b"EFI PART".as_slice()) {
        return Err(InspectionError::InvalidGpt(
            "brak primary GPT w rozpoznanym układzie RAWNAND/FULL NAND",
        ));
    }
    Ok(&source[raw_offset..])
}

fn parse_resize_gpt(raw: &[u8]) -> Result<ResizeGpt, InspectionError> {
    if raw.len() % LOGICAL_SECTOR_BYTES as usize != 0 {
        return Err(InspectionError::InvalidGpt(
            "obraz nie kończy się na granicy sektora",
        ));
    }
    let mut header = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    header.copy_from_slice(
        raw.get(LOGICAL_SECTOR_BYTES as usize..2 * LOGICAL_SECTOR_BYTES as usize)
            .ok_or(InspectionError::ImageTooSmall)?,
    );
    if &header[..8] != b"EFI PART" {
        return Err(InspectionError::InvalidGpt("brak sygnatury EFI PART"));
    }
    let header_size = read_u32(&header, 12)? as usize;
    if !(GPT_MIN_HEADER_SIZE..=header.len()).contains(&header_size)
        || read_u32(&header, 16)? != crc32_with_zeroed_field(&header[..header_size], 16, 4)
        || read_u64(&header, 24)? != GPT_HEADER_LBA
    {
        return Err(InspectionError::InvalidGpt("niepoprawny primary GPT"));
    }
    let entries_lba = read_u64(&header, 72)?;
    let entry_count = read_u32(&header, 80)? as usize;
    let entry_size = read_u32(&header, 84)? as usize;
    if entry_count == 0 || entry_size < GPT_ENTRY_MIN_SIZE {
        return Err(InspectionError::InvalidGpt(
            "nieprawidłowy opis tablicy partycji",
        ));
    }
    let table_len = entry_count
        .checked_mul(entry_size)
        .filter(|length| *length <= MAX_GPT_TABLE_BYTES)
        .ok_or(InspectionError::InvalidGpt(
            "tablica partycji jest zbyt duża",
        ))?;
    let table_offset = usize::try_from(
        entries_lba
            .checked_mul(LOGICAL_SECTOR_BYTES)
            .ok_or(InspectionError::InvalidGpt("przepełnienie offsetu tablicy"))?,
    )
    .map_err(|_| InspectionError::InvalidGpt("przepełnienie offsetu tablicy"))?;
    let table = raw
        .get(table_offset..table_offset + table_len)
        .ok_or(InspectionError::InvalidGpt("tablica wychodzi poza obraz"))?
        .to_vec();
    if crc32fast::hash(&table) != read_u32(&header, 88)? {
        return Err(InspectionError::InvalidGpt(
            "niepoprawne CRC tablicy partycji",
        ));
    }
    let mut user = None;
    let mut highest_last_lba = 0_u64;
    for (index, entry) in table.chunks_exact(entry_size).enumerate() {
        if entry[..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let first_lba = read_u64(entry, 32)?;
        let last_lba = read_u64(entry, 40)?;
        if first_lba > last_lba {
            return Err(InspectionError::InvalidGpt(
                "zakres partycji jest nieprawidłowy",
            ));
        }
        highest_last_lba = highest_last_lba.max(last_lba);
        if decode_gpt_name(&entry[56..GPT_ENTRY_MIN_SIZE]) == "USER" {
            user = Some((index * entry_size, first_lba, last_lba));
        }
    }
    let (user_entry_offset, user_first_lba, user_last_lba) =
        user.ok_or(InspectionError::MissingUserPartition)?;
    if user_last_lba != highest_last_lba {
        return Err(InspectionError::InvalidGpt(
            "USER nie jest ostatnią partycją",
        ));
    }
    Ok(ResizeGpt {
        header,
        header_size,
        table,
        entries_lba,
        user_entry_offset,
        user_first_lba,
        user_last_lba,
    })
}

fn expanded_gpt_metadata(
    gpt: &ResizeGpt,
    target_sectors: u64,
    target_user_last_lba: u64,
) -> Result<([u8; 512], Vec<u8>, [u8; 512], u64), SyntheticRestoreError> {
    let table_sectors = (gpt.table.len() as u64)
        .checked_add(LOGICAL_SECTOR_BYTES - 1)
        .ok_or(SyntheticRestoreError::TargetTooLarge)?
        / LOGICAL_SECTOR_BYTES;
    let backup_table_lba = target_sectors
        .checked_sub(1)
        .and_then(|value| value.checked_sub(table_sectors))
        .ok_or(SyntheticRestoreError::TargetTooSmall)?;
    let last_usable_lba = backup_table_lba
        .checked_sub(1)
        .ok_or(SyntheticRestoreError::TargetTooSmall)?;
    if target_user_last_lba > last_usable_lba || backup_table_lba <= gpt.entries_lba {
        return Err(SyntheticRestoreError::TargetTooSmall);
    }
    let mut table = gpt.table.clone();
    table[gpt.user_entry_offset + 40..gpt.user_entry_offset + 48]
        .copy_from_slice(&target_user_last_lba.to_le_bytes());
    let table_crc = crc32fast::hash(&table);
    let mut primary = gpt.header;
    put_gpt_header_fields(
        &mut primary,
        gpt.header_size,
        GPT_HEADER_LBA,
        target_sectors - 1,
        gpt.entries_lba,
        last_usable_lba,
        table_crc,
    );
    let mut backup = gpt.header;
    put_gpt_header_fields(
        &mut backup,
        gpt.header_size,
        target_sectors - 1,
        GPT_HEADER_LBA,
        backup_table_lba,
        last_usable_lba,
        table_crc,
    );
    Ok((primary, table, backup, backup_table_lba))
}

fn put_gpt_header_fields(
    header: &mut [u8; 512],
    header_size: usize,
    current_lba: u64,
    backup_lba: u64,
    entries_lba: u64,
    last_usable_lba: u64,
    table_crc: u32,
) {
    header[24..32].copy_from_slice(&current_lba.to_le_bytes());
    header[32..40].copy_from_slice(&backup_lba.to_le_bytes());
    header[48..56].copy_from_slice(&last_usable_lba.to_le_bytes());
    header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    header[88..92].copy_from_slice(&table_crc.to_le_bytes());
    header[16..20].fill(0);
    let crc = crc32_with_zeroed_field(&header[..header_size], 16, 4);
    header[16..20].copy_from_slice(&crc.to_le_bytes());
}

fn write_gpt_table(image: &mut [u8], lba: u64, table: &[u8]) -> Result<(), SyntheticRestoreError> {
    let offset = usize::try_from(
        lba.checked_mul(LOGICAL_SECTOR_BYTES)
            .ok_or(SyntheticRestoreError::TargetTooLarge)?,
    )
    .map_err(|_| SyntheticRestoreError::TargetTooLarge)?;
    let rounded_len =
        table.len().div_ceil(LOGICAL_SECTOR_BYTES as usize) * LOGICAL_SECTOR_BYTES as usize;
    let destination = image
        .get_mut(offset..offset + rounded_len)
        .ok_or(SyntheticRestoreError::TargetTooSmall)?;
    destination.fill(0);
    destination[..table.len()].copy_from_slice(table);
    Ok(())
}

fn write_sector(
    image: &mut [u8],
    lba: u64,
    sector: &[u8; 512],
) -> Result<(), SyntheticRestoreError> {
    let offset = usize::try_from(
        lba.checked_mul(LOGICAL_SECTOR_BYTES)
            .ok_or(SyntheticRestoreError::TargetTooLarge)?,
    )
    .map_err(|_| SyntheticRestoreError::TargetTooLarge)?;
    image
        .get_mut(offset..offset + sector.len())
        .ok_or(SyntheticRestoreError::TargetTooSmall)?
        .copy_from_slice(sector);
    Ok(())
}

fn read_user_plain(
    image: &[u8],
    user_offset: u64,
    user_byte_len: u64,
    key: &BisKey,
    offset: u64,
    output: &mut [u8],
) -> Result<(), SyntheticRestoreError> {
    let end = offset
        .checked_add(output.len() as u64)
        .ok_or(SyntheticRestoreError::TargetTooSmall)?;
    if end > user_byte_len {
        return Err(SyntheticRestoreError::TargetTooSmall);
    }
    let crypto = SwitchAesXts128::new(key);
    let mut cursor = offset;
    let mut written = 0;
    while written < output.len() {
        let unit_index = cursor / BIS_DATA_UNIT_BYTES as u64;
        let within = (cursor % BIS_DATA_UNIT_BYTES as u64) as usize;
        let wanted = (output.len() - written).min(BIS_DATA_UNIT_BYTES - within);
        let unit_offset = user_offset
            .checked_add(
                unit_index
                    .checked_mul(BIS_DATA_UNIT_BYTES as u64)
                    .ok_or(SyntheticRestoreError::TargetTooLarge)?,
            )
            .ok_or(SyntheticRestoreError::TargetTooLarge)?;
        let start =
            usize::try_from(unit_offset).map_err(|_| SyntheticRestoreError::TargetTooLarge)?;
        let mut unit = image
            .get(start..start + BIS_DATA_UNIT_BYTES)
            .ok_or(SyntheticRestoreError::TargetTooSmall)?
            .to_vec();
        crypto
            .decrypt_unit(unit_index, &mut unit)
            .expect("fixed unit length");
        output[written..written + wanted].copy_from_slice(&unit[within..within + wanted]);
        cursor += wanted as u64;
        written += wanted;
    }
    Ok(())
}

fn write_user_plain(
    image: &mut [u8],
    user_offset: u64,
    user_byte_len: u64,
    key: &BisKey,
    offset: u64,
    input: &[u8],
) -> Result<(), SyntheticRestoreError> {
    let end = offset
        .checked_add(input.len() as u64)
        .ok_or(SyntheticRestoreError::TargetTooSmall)?;
    if end > user_byte_len {
        return Err(SyntheticRestoreError::TargetTooSmall);
    }
    let crypto = SwitchAesXts128::new(key);
    let mut cursor = offset;
    let mut consumed = 0;
    while consumed < input.len() {
        let unit_index = cursor / BIS_DATA_UNIT_BYTES as u64;
        let within = (cursor % BIS_DATA_UNIT_BYTES as u64) as usize;
        let wanted = (input.len() - consumed).min(BIS_DATA_UNIT_BYTES - within);
        let unit_offset = user_offset
            .checked_add(
                unit_index
                    .checked_mul(BIS_DATA_UNIT_BYTES as u64)
                    .ok_or(SyntheticRestoreError::TargetTooLarge)?,
            )
            .ok_or(SyntheticRestoreError::TargetTooLarge)?;
        let start =
            usize::try_from(unit_offset).map_err(|_| SyntheticRestoreError::TargetTooLarge)?;
        let destination = image
            .get_mut(start..start + BIS_DATA_UNIT_BYTES)
            .ok_or(SyntheticRestoreError::TargetTooSmall)?;
        let mut unit = destination.to_vec();
        crypto
            .decrypt_unit(unit_index, &mut unit)
            .expect("fixed unit length");
        unit[within..within + wanted].copy_from_slice(&input[consumed..consumed + wanted]);
        crypto
            .encrypt_unit(unit_index, &mut unit)
            .expect("fixed unit length");
        destination.copy_from_slice(&unit);
        cursor += wanted as u64;
        consumed += wanted;
    }
    Ok(())
}

fn fat32_cluster_count(boot_sector: &Fat32BootSector) -> Result<u32, UserFilesystemError> {
    let data_start = u64::from(boot_sector.reserved_sectors)
        + u64::from(boot_sector.fat_count) * u64::from(boot_sector.sectors_per_fat);
    let cluster_count = (u64::from(boot_sector.total_sectors)
        .checked_sub(data_start)
        .ok_or(UserFilesystemError::InvalidBootSector(
            "nieprawidłowy początek danych FAT32",
        ))?)
        / u64::from(boot_sector.sectors_per_cluster);
    u32::try_from(cluster_count).map_err(|_| {
        UserFilesystemError::InvalidBootSector("liczba klastrów przekracza limit FAT32")
    })
}

fn fat_entry(fat: &[u8], cluster: u32) -> Result<u32, UserFilesystemError> {
    let offset = usize::try_from(cluster)
        .ok()
        .and_then(|cluster| cluster.checked_mul(4))
        .ok_or(UserFilesystemError::InvalidBootSector(
            "przepełnienie wpisu FAT",
        ))?;
    let entry = fat
        .get(offset..offset + 4)
        .ok_or(UserFilesystemError::InvalidBootSector(
            "tablica FAT jest za krótka",
        ))?;
    Ok(u32::from_le_bytes(entry.try_into().expect("fixed FAT entry")) & 0x0fff_ffff)
}

/// Calculates the FAT32 geometry after expanding `USER`, without modifying an
/// image. The fixed point is necessary because a larger FAT itself shifts the
/// data area and changes the number of addressable clusters.
pub fn plan_fat32_expansion(
    boot_sector: &Fat32BootSector,
    target_user_sectors: u64,
) -> Result<Fat32ExpansionPlan, Fat32PlanError> {
    let current_user_sectors = u64::from(boot_sector.total_sectors);
    if target_user_sectors <= current_user_sectors {
        return Err(Fat32PlanError::TargetNotLarger);
    }
    let target_user_sectors =
        u32::try_from(target_user_sectors).map_err(|_| Fat32PlanError::CapacityTooLarge)?;
    let fixed_sectors = u64::from(boot_sector.reserved_sectors);
    let fat_count = u64::from(boot_sector.fat_count);
    let cluster_sectors = u64::from(boot_sector.sectors_per_cluster);
    if fat_count != 2 || cluster_sectors == 0 {
        return Err(Fat32PlanError::InvalidGeometry);
    }

    let current_data_start = fixed_sectors
        .checked_add(fat_count * u64::from(boot_sector.sectors_per_fat))
        .ok_or(Fat32PlanError::InvalidGeometry)?;
    let current_cluster_count = (current_user_sectors
        .checked_sub(current_data_start)
        .ok_or(Fat32PlanError::InvalidGeometry)?)
        / cluster_sectors;

    let mut target_sectors_per_fat = u64::from(boot_sector.sectors_per_fat);
    for _ in 0..64 {
        let target_data_start = fixed_sectors
            .checked_add(fat_count * target_sectors_per_fat)
            .ok_or(Fat32PlanError::InvalidGeometry)?;
        let target_cluster_count = (u64::from(target_user_sectors)
            .checked_sub(target_data_start)
            .ok_or(Fat32PlanError::InvalidGeometry)?)
            / cluster_sectors;
        let required_entries = target_cluster_count
            .checked_add(2)
            .ok_or(Fat32PlanError::CapacityTooLarge)?;
        let required_bytes = required_entries
            .checked_mul(4)
            .ok_or(Fat32PlanError::CapacityTooLarge)?;
        let required_sectors = required_bytes
            .checked_add(LOGICAL_SECTOR_BYTES - 1)
            .ok_or(Fat32PlanError::CapacityTooLarge)?
            / LOGICAL_SECTOR_BYTES;
        if required_sectors != target_sectors_per_fat {
            target_sectors_per_fat = required_sectors;
            continue;
        }
        let target_data_start =
            u32::try_from(target_data_start).map_err(|_| Fat32PlanError::CapacityTooLarge)?;
        let current_data_start =
            u32::try_from(current_data_start).map_err(|_| Fat32PlanError::CapacityTooLarge)?;
        let target_cluster_count =
            u32::try_from(target_cluster_count).map_err(|_| Fat32PlanError::CapacityTooLarge)?;
        let current_cluster_count =
            u32::try_from(current_cluster_count).map_err(|_| Fat32PlanError::CapacityTooLarge)?;
        let target_sectors_per_fat =
            u32::try_from(target_sectors_per_fat).map_err(|_| Fat32PlanError::CapacityTooLarge)?;
        return Ok(Fat32ExpansionPlan {
            current_user_sectors: boot_sector.total_sectors,
            target_user_sectors,
            current_sectors_per_fat: boot_sector.sectors_per_fat,
            target_sectors_per_fat,
            current_data_start_sector: current_data_start,
            target_data_start_sector: target_data_start,
            data_start_shift_sectors: target_data_start
                .checked_sub(current_data_start)
                .ok_or(Fat32PlanError::InvalidGeometry)?,
            current_cluster_count,
            target_cluster_count,
        });
    }
    Err(Fat32PlanError::DidNotConverge)
}

/// Given `rawnand.bin.00` (or any later numeric part), resolves all contiguous
/// siblings starting at `.00`. A regular filename is treated as a single image.
fn split_paths(input: &Path) -> Result<Vec<PathBuf>, InspectionError> {
    let Some((prefix, width)) = numeric_suffix(input) else {
        return Ok(vec![input.to_path_buf()]);
    };
    let mut paths = Vec::new();
    let mut index = 0_usize;
    loop {
        let candidate = PathBuf::from(format!("{prefix}{index:0width$}"));
        if candidate.is_file() {
            paths.push(candidate);
            index += 1;
            continue;
        }
        if index == 0 || PathBuf::from(format!("{prefix}{:0width$}", index + 1)).is_file() {
            return Err(InspectionError::MissingSplitPart);
        }
        break;
    }
    Ok(paths)
}

fn numeric_suffix(path: &Path) -> Option<(String, usize)> {
    let filename = path.file_name()?.to_str()?;
    let (base, suffix) = filename.rsplit_once('.')?;
    if suffix.len() < 2 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    Some((
        parent
            .join(format!("{base}."))
            .to_string_lossy()
            .into_owned(),
        suffix.len(),
    ))
}

/// Inspects an explicitly recognised RAWNAND or FULL NAND image without
/// touching a target device. FULL NAND is only recognised when its primary GPT
/// is exactly after the two fixed 4 MiB boot areas; this deliberately avoids
/// searching arbitrary data for a GPT signature.
pub fn inspect_nand_image(path: impl AsRef<Path>) -> Result<RawNandInspection, InspectionError> {
    let image = ImageReader::open(path)?;
    inspect_nand_reader(&image)
}

fn inspect_nand_reader(image: &ImageReader) -> Result<RawNandInspection, InspectionError> {
    if image.byte_len < (GPT_HEADER_LBA + 1) * LOGICAL_SECTOR_BYTES {
        return Err(InspectionError::ImageTooSmall);
    }

    let mut raw_header = [0_u8; 8];
    image.read_exact_at(GPT_HEADER_LBA * LOGICAL_SECTOR_BYTES, &mut raw_header)?;
    let (format, raw_nand_offset) = if &raw_header == b"EFI PART" {
        (NandImageFormat::RawNand, 0)
    } else {
        let full_gpt_offset = FULL_NAND_BOOT_AREA_BYTES
            .checked_add(GPT_HEADER_LBA * LOGICAL_SECTOR_BYTES)
            .ok_or(InspectionError::ImageTooSmall)?;
        if image.byte_len < full_gpt_offset + raw_header.len() as u64 {
            return Err(InspectionError::ImageTooSmall);
        }
        image.read_exact_at(full_gpt_offset, &mut raw_header)?;
        if &raw_header != b"EFI PART" {
            return Err(InspectionError::InvalidGpt(
                "brak primary GPT w rozpoznanym układzie RAWNAND/FULL NAND",
            ));
        }
        (NandImageFormat::FullNand, FULL_NAND_BOOT_AREA_BYTES)
    };
    let image_byte_len = image
        .byte_len
        .checked_sub(raw_nand_offset)
        .ok_or(InspectionError::ImageTooSmall)?;
    inspect_gpt_at(&image, format, raw_nand_offset, image_byte_len)
}

/// Legacy name retained for callers that only need the inspection result. It
/// now recognises both supported containers; callers should use
/// `inspect_nand_image` in new code.
pub fn inspect_raw_nand_image(
    path: impl AsRef<Path>,
) -> Result<RawNandInspection, InspectionError> {
    inspect_nand_image(path)
}

fn inspect_gpt_at(
    image: &ImageReader,
    format: NandImageFormat,
    raw_nand_offset: u64,
    image_byte_len: u64,
) -> Result<RawNandInspection, InspectionError> {
    if image_byte_len < (GPT_HEADER_LBA + 1) * LOGICAL_SECTOR_BYTES {
        return Err(InspectionError::ImageTooSmall);
    }
    let mut sector = [0_u8; LOGICAL_SECTOR_BYTES as usize];
    let header_offset = raw_nand_offset
        .checked_add(GPT_HEADER_LBA * LOGICAL_SECTOR_BYTES)
        .ok_or(InspectionError::ImageTooSmall)?;
    image.read_exact_at(header_offset, &mut sector)?;
    if &sector[0..8] != b"EFI PART" {
        return Err(InspectionError::InvalidGpt("brak sygnatury EFI PART"));
    }

    let header_size = read_u32(&sector, 12)? as usize;
    if !(GPT_MIN_HEADER_SIZE..=sector.len()).contains(&header_size) {
        return Err(InspectionError::InvalidGpt(
            "nieprawidłowy rozmiar nagłówka",
        ));
    }
    if read_u32(&sector, 16)? != crc32_with_zeroed_field(&sector[..header_size], 16, 4) {
        return Err(InspectionError::InvalidGpt("niepoprawne CRC nagłówka"));
    }
    if read_u64(&sector, 24)? != GPT_HEADER_LBA {
        return Err(InspectionError::InvalidGpt("nagłówek nie jest primary GPT"));
    }

    let backup_gpt_lba = read_u64(&sector, 32)?;
    let first_usable_lba = read_u64(&sector, 40)?;
    let last_usable_lba = read_u64(&sector, 48)?;
    let entries_lba = read_u64(&sector, 72)?;
    let entry_count = read_u32(&sector, 80)? as usize;
    let entry_size = read_u32(&sector, 84)? as usize;
    let entries_crc32 = read_u32(&sector, 88)?;
    if entry_count == 0 || entry_size < GPT_ENTRY_MIN_SIZE {
        return Err(InspectionError::InvalidGpt(
            "nieprawidłowy opis tablicy partycji",
        ));
    }
    let table_len = entry_count
        .checked_mul(entry_size)
        .filter(|length| *length <= MAX_GPT_TABLE_BYTES)
        .ok_or(InspectionError::InvalidGpt(
            "tablica partycji jest zbyt duża",
        ))?;
    let table_relative_offset = entries_lba
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .ok_or(InspectionError::InvalidGpt("przepełnienie offsetu tablicy"))?;
    let table_end =
        table_relative_offset
            .checked_add(table_len as u64)
            .ok_or(InspectionError::InvalidGpt(
                "przepełnienie rozmiaru tablicy",
            ))?;
    if table_end > image_byte_len {
        return Err(InspectionError::InvalidGpt("tablica wychodzi poza obraz"));
    }
    let primary_metadata_end =
        first_usable_lba
            .checked_mul(LOGICAL_SECTOR_BYTES)
            .ok_or(InspectionError::InvalidGpt(
                "przepełnienie końca primary GPT",
            ))?;
    if table_relative_offset > primary_metadata_end || table_end > primary_metadata_end {
        return Err(InspectionError::InvalidGpt(
            "tablica primary GPT wychodzi poza obszar metadanych",
        ));
    }
    let table_offset = raw_nand_offset
        .checked_add(table_relative_offset)
        .ok_or(InspectionError::InvalidGpt("przepełnienie offsetu tablicy"))?;

    let mut table = vec![0_u8; table_len];
    image.read_exact_at(table_offset, &mut table)?;
    if crc32fast::hash(&table) != entries_crc32 {
        return Err(InspectionError::InvalidGpt(
            "niepoprawne CRC tablicy partycji",
        ));
    }

    let mut partitions = Vec::new();
    for entry in table.chunks_exact(entry_size) {
        if entry[0..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let first_lba = read_u64(entry, 32)?;
        let last_lba = read_u64(entry, 40)?;
        if first_lba < first_usable_lba || last_lba > last_usable_lba || first_lba > last_lba {
            return Err(InspectionError::InvalidGpt(
                "zakres partycji jest nieprawidłowy",
            ));
        }
        let name = decode_gpt_name(&entry[56..GPT_ENTRY_MIN_SIZE]);
        partitions.push(GptPartition {
            name,
            first_lba,
            last_lba,
        });
    }
    let user_partition = partitions
        .iter()
        .find(|partition| partition.name == "USER")
        .cloned()
        .ok_or(InspectionError::MissingUserPartition)?;
    let primary_metadata_byte_len = first_usable_lba
        .checked_mul(LOGICAL_SECTOR_BYTES)
        .filter(|length| *length > GPT_HEADER_LBA * LOGICAL_SECTOR_BYTES)
        .filter(|length| *length <= image_byte_len)
        .ok_or(InspectionError::InvalidGpt(
            "nieprawidłowy koniec primary GPT",
        ))?;

    Ok(RawNandInspection {
        image_byte_len,
        container_byte_len: image.byte_len,
        source_part_count: image.parts.len(),
        format,
        raw_nand_offset_bytes: raw_nand_offset,
        primary_metadata_byte_len,
        boot0_source: if format == NandImageFormat::FullNand {
            BootComponentSource::Embedded
        } else {
            BootComponentSource::Absent
        },
        boot1_source: if format == NandImageFormat::FullNand {
            BootComponentSource::Embedded
        } else {
            BootComponentSource::Absent
        },
        backup_gpt_lba,
        partitions,
        user_partition,
    })
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, InspectionError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(InspectionError::InvalidGpt("ucięte pole 32-bitowe"))?;
    Ok(u32::from_le_bytes(
        value.try_into().expect("fixed slice length"),
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, InspectionError> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or(InspectionError::InvalidGpt("ucięte pole 64-bitowe"))?;
    Ok(u64::from_le_bytes(
        value.try_into().expect("fixed slice length"),
    ))
}

fn crc32_with_zeroed_field(bytes: &[u8], offset: usize, field_len: usize) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(&bytes[..offset]);
    hasher.update(&vec![0; field_len]);
    hasher.update(&bytes[offset + field_len..]);
    hasher.finalize()
}

fn decode_gpt_name(bytes: &[u8]) -> String {
    let words = bytes
        .chunks_exact(2)
        .map(|word| u16::from_le_bytes([word[0], word[1]]))
        .take_while(|word| *word != 0)
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&words)
}

/// Produces a non-destructive operation plan. A plan alone never authorizes a
/// write: the device writer additionally requires a repeated source preflight,
/// target re-identification, mount checks, an exclusive lock and the exact
/// typed device-path confirmation.
pub fn create_operation_plan(
    mode: OperationMode,
    backup: ArtifactReceipt,
    keyset: Option<ArtifactReceipt>,
    boot0: Option<ArtifactReceipt>,
    boot1: Option<ArtifactReceipt>,
    target: BlockDevice,
) -> Result<OperationPlan, OperationPlanError> {
    if backup.byte_len == 0
        || keyset
            .as_ref()
            .is_some_and(|artifact| artifact.byte_len == 0)
        || boot0
            .as_ref()
            .is_some_and(|artifact| artifact.byte_len == 0)
        || boot1
            .as_ref()
            .is_some_and(|artifact| artifact.byte_len == 0)
    {
        return Err(OperationPlanError::EmptyArtifact);
    }
    if backup.byte_len > MAX_ARTIFACT_BYTES {
        return Err(OperationPlanError::ArtifactTooLarge);
    }
    if keyset
        .as_ref()
        .is_some_and(|artifact| artifact.byte_len > MAX_KEYSET_BYTES)
    {
        return Err(OperationPlanError::KeysetTooLarge);
    }
    if backup.kind != ArtifactKind::Backup
        || keyset
            .as_ref()
            .is_some_and(|artifact| artifact.kind != ArtifactKind::Keyset)
        || boot0
            .as_ref()
            .is_some_and(|artifact| artifact.kind != ArtifactKind::Boot0)
        || boot1
            .as_ref()
            .is_some_and(|artifact| artifact.kind != ArtifactKind::Boot1)
    {
        return Err(OperationPlanError::ArtifactKindsDoNotMatch);
    }
    if mode == OperationMode::RestoreAndExpandUser && keyset.is_none() {
        return Err(OperationPlanError::KeysetRequired);
    }
    if boot0.is_some() != boot1.is_some() {
        return Err(OperationPlanError::IncompleteBootBackup);
    }
    if target.is_read_only {
        return Err(OperationPlanError::TargetReadOnly);
    }

    let write_operations_enabled = cfg!(target_os = "linux") && boot0.is_none() && boot1.is_none();
    Ok(OperationPlan {
        mode,
        target,
        backup,
        keyset,
        boot0,
        boot1,
        requires_read_only_preflight: true,
        write_operations_enabled,
    })
}

/// Re-identifies a selected target without opening it for writing. This is
/// intentionally separate from source/GPT preflight so adapters can display
/// a concrete mount or confirmation failure before enabling a destructive
/// action.
pub fn list_block_devices() -> io::Result<Vec<BlockDevice>> {
    device::list_block_devices()
}

pub fn preflight_restore_target(
    plan: &OperationPlan,
    backup_path: impl AsRef<Path>,
) -> Result<RestoreTargetPreflight, RestoreTargetSafetyError> {
    let (canonical_path, target) = current_physical_target(plan)?;
    if paths_name_same_device(backup_path.as_ref(), Path::new(&canonical_path)) {
        return Err(RestoreTargetSafetyError::SourceMatchesTarget);
    }
    let target_name = Path::new(&canonical_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RestoreTargetSafetyError::TargetNotPhysicalDevice)?;
    let mounted_at = mounted_at_for_target(target_name)
        .map_err(|_| RestoreTargetSafetyError::TargetUnavailable)?;
    if !mounted_at.is_empty() {
        return Err(RestoreTargetSafetyError::TargetMounted);
    }
    Ok(RestoreTargetPreflight {
        required_confirmation: canonical_path.clone(),
        canonical_path,
        byte_len: target.byte_len,
        mounted_at,
    })
}

/// Validates the exact destructive confirmation and acquires an exclusive,
/// read-only descriptor lock. The returned token is only a safety gate: it
/// deliberately cannot write data, because the durable restore engine is not
/// part of this increment yet.
pub fn authorize_restore_target(
    plan: &OperationPlan,
    backup_path: impl AsRef<Path>,
    typed_confirmation: &str,
) -> Result<AuthorizedRestoreTarget, RestoreTargetSafetyError> {
    let preflight = preflight_restore_target(plan, backup_path.as_ref())?;
    if typed_confirmation != preflight.required_confirmation {
        return Err(RestoreTargetSafetyError::ConfirmationMismatch);
    }
    let file =
        File::open(&preflight.canonical_path).map_err(|_| RestoreTargetSafetyError::CannotLock)?;
    file.try_lock_exclusive()
        .map_err(|_| RestoreTargetSafetyError::CannotLock)?;

    // A mount can appear between the first check and the lock. Re-run all
    // checks while holding the descriptor, and release it on failure.
    if let Err(error) = preflight_restore_target(plan, backup_path) {
        let _ = FileExt::unlock(&file);
        return Err(error);
    }
    Ok(AuthorizedRestoreTarget {
        file,
        canonical_path: preflight.canonical_path,
        byte_len: preflight.byte_len,
    })
}

fn current_physical_target(
    plan: &OperationPlan,
) -> Result<(String, BlockDevice), RestoreTargetSafetyError> {
    current_physical_block(&plan.target)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UserExpansionPlan {
    pub current_user_sectors: u64,
    pub target_user_sectors: u64,
    pub gained_sectors: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    TargetEndsBeforeUser,
    NoCapacityGain,
}

/// Calculates the full-capacity `USER` size in the same GPT address space as
/// `target_capacity_sectors`. Callers must first account for a FULL NAND's
/// BOOT area; this function does not infer physical offsets.
pub fn plan_user_expansion(
    target_capacity_sectors: u64,
    user_start_lba: u64,
    current_user_sectors: u64,
) -> Result<UserExpansionPlan, PlanError> {
    let reserved_end = user_start_lba
        .checked_add(BACKUP_GPT_SECTORS)
        .ok_or(PlanError::TargetEndsBeforeUser)?;
    let available = target_capacity_sectors
        .checked_sub(reserved_end)
        .ok_or(PlanError::TargetEndsBeforeUser)?;
    let target_user_sectors = available / USER_CLUSTER_SECTORS * USER_CLUSTER_SECTORS;

    if target_user_sectors <= current_user_sectors {
        return Err(PlanError::NoCapacityGain);
    }

    Ok(UserExpansionPlan {
        current_user_sectors,
        target_user_sectors,
        gained_sectors: target_user_sectors - current_user_sectors,
    })
}

pub fn engine_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const TEST_USER_KEYSET: &str =
        "bis_key_02 = 2718281828459045235360287471352631415926535897932384626433832795";

    #[cfg(target_os = "linux")]
    #[test]
    fn mountinfo_path_defaults_to_the_current_mount_namespace() {
        assert_eq!(
            mountinfo_path_from_environment(None),
            PathBuf::from("/proc/self/mountinfo")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mountinfo_path_accepts_an_explicit_host_bind_mount() {
        assert_eq!(
            mountinfo_path_from_environment(Some("/run/nandunx/host-mountinfo".into())),
            PathBuf::from("/run/nandunx/host-mountinfo")
        );
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn image_reader_keeps_open_source_when_path_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.00");
        let replacement_path = directory.path().join("replacement");
        fs::write(&source_path, [0xA5; 512]).unwrap();
        let reader = ImageReader::open(&source_path).unwrap();
        fs::write(&replacement_path, [0x5A; 512]).unwrap();
        fs::rename(replacement_path, source_path).unwrap();
        let mut actual = [0; 512];
        reader.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, [0xA5; 512]);
    }

    pub(crate) fn raw_nand_fixture() -> tempfile::NamedTempFile {
        let sector_len = LOGICAL_SECTOR_BYTES as usize;
        let backup_lba = 255_u64;
        let mut image = vec![0_u8; (backup_lba as usize + 1) * sector_len];
        let table_offset = 2 * sector_len;
        let table = &mut image[table_offset..table_offset + 4 * GPT_ENTRY_MIN_SIZE];
        table[0] = 1; // non-zero partition-type GUID
        put_u64(table, 32, 34);
        put_u64(table, 40, 225);
        for (index, word) in "USER".encode_utf16().enumerate() {
            table[56 + index * 2..58 + index * 2].copy_from_slice(&word.to_le_bytes());
        }
        let table_crc = crc32fast::hash(table);

        let header = &mut image[sector_len..2 * sector_len];
        header[0..8].copy_from_slice(b"EFI PART");
        put_u32(header, 8, 0x0001_0000);
        put_u32(header, 12, GPT_MIN_HEADER_SIZE as u32);
        put_u64(header, 24, GPT_HEADER_LBA);
        put_u64(header, 32, backup_lba);
        put_u64(header, 40, 34);
        put_u64(header, 48, 225);
        put_u64(header, 72, 2);
        put_u32(header, 80, 4);
        put_u32(header, 84, GPT_ENTRY_MIN_SIZE as u32);
        put_u32(header, 88, table_crc);
        put_u32(
            header,
            16,
            crc32_with_zeroed_field(&header[..GPT_MIN_HEADER_SIZE], 16, 4),
        );

        let mut user_first_unit = vec![0_u8; BIS_DATA_UNIT_BYTES];
        let boot_sector = &mut user_first_unit[..LOGICAL_SECTOR_BYTES as usize];
        boot_sector[11..13].copy_from_slice(&(LOGICAL_SECTOR_BYTES as u16).to_le_bytes());
        boot_sector[13] = USER_CLUSTER_SECTORS as u8;
        boot_sector[14..16].copy_from_slice(&32_u16.to_le_bytes());
        boot_sector[16] = 2;
        boot_sector[19..21].copy_from_slice(&192_u16.to_le_bytes());
        boot_sector[21] = 0xf8;
        boot_sector[36..40].copy_from_slice(&1_u32.to_le_bytes());
        boot_sector[50..52].copy_from_slice(&6_u16.to_le_bytes());
        boot_sector[82..90].copy_from_slice(b"FAT32   ");
        boot_sector[510..512].copy_from_slice(&[0x55, 0xaa]);
        let boot_sector_copy = boot_sector.to_vec();
        user_first_unit[6 * LOGICAL_SECTOR_BYTES as usize..7 * LOGICAL_SECTOR_BYTES as usize]
            .copy_from_slice(&boot_sector_copy);
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let crypto = SwitchAesXts128::new(keyset.user_key().unwrap());
        crypto.encrypt_unit(0, &mut user_first_unit).unwrap();
        let user_offset = 34 * LOGICAL_SECTOR_BYTES as usize;
        image[user_offset..user_offset + BIS_DATA_UNIT_BYTES].copy_from_slice(&user_first_unit);

        let mut fat_unit = vec![0_u8; BIS_DATA_UNIT_BYTES];
        put_u32(&mut fat_unit, 0, 0x0fff_fff8);
        put_u32(&mut fat_unit, 4, 0x0fff_ffff);
        put_u32(&mut fat_unit, 8, 0x0fff_ffff);
        let first_fat = fat_unit[..LOGICAL_SECTOR_BYTES as usize].to_vec();
        fat_unit[LOGICAL_SECTOR_BYTES as usize..2 * LOGICAL_SECTOR_BYTES as usize]
            .copy_from_slice(&first_fat);
        crypto.encrypt_unit(1, &mut fat_unit).unwrap();
        let fat_offset = user_offset + BIS_DATA_UNIT_BYTES;
        image[fat_offset..fat_offset + BIS_DATA_UNIT_BYTES].copy_from_slice(&fat_unit);

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&image).unwrap();
        file.flush().unwrap();
        file
    }

    pub(crate) fn user_keyset_fixture() -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(TEST_USER_KEYSET.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    fn full_nand_fixture() -> tempfile::NamedTempFile {
        let raw = raw_nand_fixture();
        let raw_bytes = fs::read(raw.path()).unwrap();
        let mut full = tempfile::NamedTempFile::new().unwrap();
        full.as_file_mut()
            .set_len(FULL_NAND_BOOT_AREA_BYTES)
            .unwrap();
        full.as_file_mut()
            .seek(SeekFrom::Start(FULL_NAND_BOOT_AREA_BYTES))
            .unwrap();
        full.write_all(&raw_bytes).unwrap();
        full.flush().unwrap();
        full
    }

    fn boot_fixture(byte_len: u64) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(byte_len).unwrap();
        file
    }

    fn restore_fixture(byte_len: u64) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(byte_len).unwrap();
        file
    }

    struct RecordedRestoreProgress {
        progress: Vec<RestoreFixtureProgress>,
        cancel_after_first_copy_chunk: bool,
    }

    impl RestoreFixtureObserver for RecordedRestoreProgress {
        fn on_progress(&mut self, progress: RestoreFixtureProgress) -> bool {
            self.progress.push(progress);
            !(self.cancel_after_first_copy_chunk
                && progress.phase == RestoreFixturePhase::Copying
                && progress.processed_bytes >= RESTORE_FIXTURE_CHUNK_BYTES as u64)
        }
    }

    struct RecordedDeviceProgress {
        progress: Vec<RestoreDeviceProgress>,
        cancel_before: Option<RestoreDevicePhase>,
    }

    impl RestoreDeviceObserver for RecordedDeviceProgress {
        fn on_progress(&mut self, progress: RestoreDeviceProgress) -> bool {
            self.progress.push(progress);
            self.cancel_before != Some(progress.phase)
        }
    }

    struct CorruptingDeviceObserver {
        target_path: PathBuf,
        offset: u64,
        corrupted: bool,
    }

    impl RestoreDeviceObserver for CorruptingDeviceObserver {
        fn on_progress(&mut self, progress: RestoreDeviceProgress) -> bool {
            if !self.corrupted
                && progress.phase == RestoreDevicePhase::VerifyingPayload
                && progress.processed_bytes == 0
            {
                let mut target = OpenOptions::new()
                    .write(true)
                    .open(&self.target_path)
                    .unwrap();
                target.seek(SeekFrom::Start(self.offset)).unwrap();
                target.write_all(&[0xff]).unwrap();
                target.sync_all().unwrap();
                self.corrupted = true;
            }
            true
        }
    }

    fn fat32_chain_fixture() -> (Fat32BootSector, Vec<u8>) {
        let boot_sector = Fat32BootSector {
            bytes_per_sector: LOGICAL_SECTOR_BYTES as u16,
            sectors_per_cluster: USER_CLUSTER_SECTORS as u8,
            reserved_sectors: 32,
            fat_count: 2,
            sectors_per_fat: 1,
            total_sectors: 192,
            media_descriptor: 0xf8,
            backup_boot_sector: 6,
        };
        let mut fat = vec![0_u8; LOGICAL_SECTOR_BYTES as usize];
        put_u32(&mut fat, 0, 0x0fff_fff8);
        put_u32(&mut fat, 4, 0x0fff_ffff);
        put_u32(&mut fat, 8, 3);
        put_u32(&mut fat, 12, 0x0fff_ffff);
        put_u32(&mut fat, 16, 0x0fff_ffff);
        (boot_sector, fat)
    }

    fn fat32_boot_sector_bytes(boot_sector: &Fat32BootSector) -> [u8; 512] {
        let mut sector = [0_u8; 512];
        sector[11..13].copy_from_slice(&boot_sector.bytes_per_sector.to_le_bytes());
        sector[13] = boot_sector.sectors_per_cluster;
        sector[14..16].copy_from_slice(&boot_sector.reserved_sectors.to_le_bytes());
        sector[16] = boot_sector.fat_count;
        sector[19..21].copy_from_slice(&(boot_sector.total_sectors as u16).to_le_bytes());
        sector[21] = boot_sector.media_descriptor;
        sector[36..40].copy_from_slice(&boot_sector.sectors_per_fat.to_le_bytes());
        sector[50..52].copy_from_slice(&boot_sector.backup_boot_sector.to_le_bytes());
        sector[82..90].copy_from_slice(b"FAT32   ");
        sector[510..512].copy_from_slice(&[0x55, 0xaa]);
        sector
    }

    #[test]
    fn consumes_capacity_but_preserves_backup_gpt_and_alignment() {
        let plan = plan_user_expansion(10_000, 1_000, 8_000).unwrap();
        assert_eq!(plan.target_user_sectors, 8_960);
        assert_eq!(plan.target_user_sectors % USER_CLUSTER_SECTORS, 0);
        assert_eq!(1_000 + plan.target_user_sectors + BACKUP_GPT_SECTORS, 9_993);
    }

    #[test]
    fn rejects_a_target_without_growth() {
        assert_eq!(
            plan_user_expansion(9_033, 1_000, 8_000),
            Err(PlanError::NoCapacityGain)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_virtual_block_devices_from_the_picker() {
        assert!(!is_physical_device_name("loop0"));
        assert!(!is_physical_device_name("zram0"));
        assert!(is_physical_device_name("sda"));
        assert!(is_physical_device_name("mmcblk0"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_mountinfo_and_matches_only_target_partitions() {
        let line = "31 22 259:2 / /media/NAND\\040backup rw,nosuid - ext4 /dev/nvme0n1p2 rw";
        assert_eq!(
            parse_mountinfo_line(line),
            Some(("259:2", "/media/NAND\\040backup", "/dev/nvme0n1p2"))
        );
        assert!(source_mentions_target("/dev/nvme0n1p2", "nvme0n1"));
        assert!(source_mentions_target("/dev/sda12", "sda"));
        assert!(!source_mentions_target("/dev/sdaa1", "sda"));
        assert_eq!(
            sysfs_parent_block_name(Path::new("/sys/devices/pci/block/nvme0n1/nvme0n1p2")),
            Some("nvme0n1")
        );
        assert_eq!(
            unescape_mountinfo_path("/media/NAND\\040backup"),
            "/media/NAND backup"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exact_source_path_is_never_an_eligible_target() {
        let backup = tempfile::NamedTempFile::new().unwrap();
        assert!(paths_name_same_device(backup.path(), backup.path()));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_planning_detail_is_logged_but_not_displayed() {
        let detail = RestoreResizeFixtureError::Inspection(InspectionError::InvalidGpt(
            "synthetic planning detail",
        ));
        let error = InPlaceExpansionError::PlanningDetail(detail);
        assert_eq!(
            error.to_string(),
            "Nie można bezpiecznie zaplanować rozszerzenia USER na tym urządzeniu."
        );
        assert!(format!("{error:?}").contains("synthetic planning detail"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_device_barrier_refuses_preflight_and_authorization() {
        let target = BlockDevice {
            path: r"\\.\PhysicalDrive9".to_owned(),
            model: "synthetic".to_owned(),
            byte_len: 64 * 1024 * 1024,
            is_removable: true,
            is_read_only: false,
            boot_partitions_available: false,
        };
        let source = tempfile::NamedTempFile::new().unwrap();
        let plan = create_operation_plan(
            OperationMode::Restore,
            ArtifactReceipt {
                id: "synthetic".to_owned(),
                kind: ArtifactKind::Backup,
                byte_len: 512,
            },
            None,
            None,
            None,
            target.clone(),
        )
        .unwrap();
        assert!(matches!(
            preflight_restore_target(&plan, source.path()),
            Err(RestoreTargetSafetyError::UnsupportedPlatform)
        ));
        assert!(matches!(
            authorize_restore_target(&plan, source.path(), &target.path),
            Err(RestoreTargetSafetyError::UnsupportedPlatform)
        ));
        let in_place = create_in_place_expansion_plan(target).unwrap();
        assert!(matches!(
            authorize_in_place_expansion_target(&in_place, &in_place.target.path),
            Err(RestoreTargetSafetyError::UnsupportedPlatform)
        ));
    }

    #[test]
    fn restore_fixture_copies_raw_nand_and_reports_verified_progress() {
        let source = raw_nand_fixture();
        let source_bytes = fs::read(source.path()).unwrap();
        let target = restore_fixture(source_bytes.len() as u64 + LOGICAL_SECTOR_BYTES);
        let mut observer = RecordedRestoreProgress {
            progress: Vec::new(),
            cancel_after_first_copy_chunk: false,
        };

        let report =
            restore_raw_nand_to_fixture(source.path(), target.path(), &mut observer).unwrap();

        assert_eq!(report.status, RestoreFixtureStatus::Completed);
        assert_eq!(report.raw_nand_bytes, source_bytes.len() as u64);
        assert_eq!(report.verified_bytes, source_bytes.len() as u64);
        assert_eq!(
            fs::read(target.path()).unwrap()[..source_bytes.len()],
            source_bytes
        );
        assert!(observer
            .progress
            .iter()
            .any(|progress| progress.phase == RestoreFixturePhase::Verifying));
    }

    #[test]
    fn restore_fixture_uses_raw_nand_offset_for_full_nand() {
        let source = full_nand_fixture();
        let source_bytes = fs::read(source.path()).unwrap();
        let expected = &source_bytes[FULL_NAND_BOOT_AREA_BYTES as usize..];
        let target = restore_fixture(expected.len() as u64);
        let mut observer = ContinueRestoreFixture;

        let report =
            restore_raw_nand_to_fixture(source.path(), target.path(), &mut observer).unwrap();

        assert_eq!(report.status, RestoreFixtureStatus::Completed);
        assert_eq!(fs::read(target.path()).unwrap(), expected);
    }

    #[test]
    fn restore_fixture_cancellation_stops_at_a_chunk_boundary_without_verification() {
        let source = raw_nand_fixture();
        let target = restore_fixture(source.as_file().metadata().unwrap().len());
        let mut observer = RecordedRestoreProgress {
            progress: Vec::new(),
            cancel_after_first_copy_chunk: true,
        };

        let report =
            restore_raw_nand_to_fixture(source.path(), target.path(), &mut observer).unwrap();

        assert_eq!(
            report.status,
            RestoreFixtureStatus::Cancelled {
                phase: RestoreFixturePhase::Copying,
                processed_bytes: RESTORE_FIXTURE_CHUNK_BYTES as u64,
            }
        );
        assert_eq!(report.verified_bytes, 0);
        assert!(observer
            .progress
            .iter()
            .all(|progress| progress.phase == RestoreFixturePhase::Copying));
    }

    #[test]
    fn restore_fixture_rejects_its_source_as_target() {
        let source = raw_nand_fixture();
        let mut observer = ContinueRestoreFixture;
        assert!(matches!(
            restore_raw_nand_to_fixture(source.path(), source.path(), &mut observer),
            Err(RestoreFixtureError::SourceMatchesTarget)
        ));
    }

    #[test]
    fn device_restore_fixture_commits_primary_metadata_only_after_payload_verification() {
        let source = raw_nand_fixture();
        let source_bytes = fs::read(source.path()).unwrap();
        let inspection = inspect_nand_image(source.path()).unwrap();
        assert_eq!(
            inspection.primary_metadata_byte_len,
            34 * LOGICAL_SECTOR_BYTES
        );
        let target = restore_fixture(source_bytes.len() as u64);
        let mut target_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(target.path())
            .unwrap();
        let mut observer = RecordedDeviceProgress {
            progress: Vec::new(),
            cancel_before: None,
        };

        let report = restore_raw_nand_to_open_target(
            &ImageReader::open(source.path()).unwrap(),
            &inspection,
            &mut target_file,
            &mut observer,
        )
        .unwrap();

        assert_eq!(report.status, RestoreDeviceStatus::Completed);
        assert_eq!(report.verified_bytes, source_bytes.len() as u64);
        assert_eq!(fs::read(target.path()).unwrap(), source_bytes);
        assert!(observer
            .progress
            .iter()
            .any(|progress| progress.phase == RestoreDevicePhase::CommittingPrimaryMetadata));
        assert!(observer
            .progress
            .iter()
            .any(|progress| progress.phase == RestoreDevicePhase::VerifyingPrimaryMetadata));
    }

    #[test]
    fn device_restore_fixture_cancellation_before_metadata_keeps_old_primary_map() {
        let source = raw_nand_fixture();
        let source_bytes = fs::read(source.path()).unwrap();
        let inspection = inspect_nand_image(source.path()).unwrap();
        let target = restore_fixture(source_bytes.len() as u64);
        let old_metadata = vec![0xa5; inspection.primary_metadata_byte_len as usize];
        {
            let mut target_file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(target.path())
                .unwrap();
            target_file.write_all(&old_metadata).unwrap();
            target_file.sync_all().unwrap();
        }
        let mut target_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(target.path())
            .unwrap();
        let mut observer = RecordedDeviceProgress {
            progress: Vec::new(),
            cancel_before: Some(RestoreDevicePhase::CommittingPrimaryMetadata),
        };

        let report = restore_raw_nand_to_open_target(
            &ImageReader::open(source.path()).unwrap(),
            &inspection,
            &mut target_file,
            &mut observer,
        )
        .unwrap();

        assert_eq!(
            report.status,
            RestoreDeviceStatus::Cancelled {
                phase: RestoreDevicePhase::CommittingPrimaryMetadata,
                processed_bytes: 0,
            }
        );
        let target_bytes = fs::read(target.path()).unwrap();
        assert_eq!(
            &target_bytes[..inspection.primary_metadata_byte_len as usize],
            old_metadata.as_slice()
        );
        assert_eq!(
            &target_bytes[inspection.primary_metadata_byte_len as usize..],
            &source_bytes[inspection.primary_metadata_byte_len as usize..]
        );
    }

    #[test]
    fn device_restore_fixture_verification_failure_does_not_commit_primary_metadata() {
        let source = raw_nand_fixture();
        let source_bytes = fs::read(source.path()).unwrap();
        let inspection = inspect_nand_image(source.path()).unwrap();
        let target = restore_fixture(source_bytes.len() as u64);
        let old_metadata = vec![0x5a; inspection.primary_metadata_byte_len as usize];
        {
            let mut target_file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(target.path())
                .unwrap();
            target_file.write_all(&old_metadata).unwrap();
            target_file.sync_all().unwrap();
        }
        let mut target_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(target.path())
            .unwrap();
        let mut observer = CorruptingDeviceObserver {
            target_path: target.path().to_path_buf(),
            offset: inspection.primary_metadata_byte_len,
            corrupted: false,
        };

        assert!(matches!(
            restore_raw_nand_to_open_target(
                &ImageReader::open(source.path()).unwrap(),
                &inspection,
                &mut target_file,
                &mut observer,
            ),
            Err(RestoreDeviceError::VerificationMismatch)
        ));
        let target_bytes = fs::read(target.path()).unwrap();
        assert_eq!(
            &target_bytes[..inspection.primary_metadata_byte_len as usize],
            old_metadata.as_slice()
        );
    }

    #[test]
    fn device_restore_fixture_uses_raw_nand_region_of_full_nand() {
        let source = full_nand_fixture();
        let source_bytes = fs::read(source.path()).unwrap();
        let expected = &source_bytes[FULL_NAND_BOOT_AREA_BYTES as usize..];
        let inspection = inspect_nand_image(source.path()).unwrap();
        let target = restore_fixture(expected.len() as u64);
        let mut target_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(target.path())
            .unwrap();
        let mut observer = ContinueRestoreDevice;

        let report = restore_raw_nand_to_open_target(
            &ImageReader::open(source.path()).unwrap(),
            &inspection,
            &mut target_file,
            &mut observer,
        )
        .unwrap();

        assert_eq!(report.status, RestoreDeviceStatus::Completed);
        assert_eq!(fs::read(target.path()).unwrap(), expected);
    }

    #[test]
    fn rejects_a_primary_gpt_table_outside_the_deferred_metadata_range() {
        let mut image = raw_nand_fixture();
        let mut header = [0_u8; LOGICAL_SECTOR_BYTES as usize];
        image
            .as_file_mut()
            .seek(SeekFrom::Start(LOGICAL_SECTOR_BYTES))
            .unwrap();
        image.as_file_mut().read_exact(&mut header).unwrap();
        put_u64(&mut header, 40, 2);
        let header_crc = crc32_with_zeroed_field(&header[..GPT_MIN_HEADER_SIZE], 16, 4);
        put_u32(&mut header, 16, header_crc);
        image
            .as_file_mut()
            .seek(SeekFrom::Start(LOGICAL_SECTOR_BYTES))
            .unwrap();
        image.as_file_mut().write_all(&header).unwrap();
        image.as_file_mut().sync_all().unwrap();

        assert!(matches!(
            inspect_nand_image(image.path()),
            Err(InspectionError::InvalidGpt(
                "tablica primary GPT wychodzi poza obszar metadanych"
            ))
        ));
    }

    #[test]
    fn resize_fixture_verifies_the_committed_gpt_and_encrypted_user() {
        let source = raw_nand_fixture();
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();

        let status = restore_and_expand_user_to_fixture(
            source.path(),
            target.path(),
            keyset.user_key().unwrap(),
            None,
        )
        .unwrap();

        assert_eq!(status, SyntheticRestoreStatus::Completed);
        let inspection = inspect_nand_image(target.path()).unwrap();
        assert_eq!(inspection.user_partition.last_lba, 9_953);
        assert!(validate_user_fat32(
            target.path(),
            0,
            &inspection.user_partition,
            keyset.user_key().unwrap()
        )
        .is_ok());
    }

    #[test]
    fn in_place_resize_reencrypts_relocated_clusters_and_preserves_plaintext() {
        let source = raw_nand_fixture();
        let source_bytes = fs::read(source.path()).unwrap();
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();
        {
            let mut target_file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(target.path())
                .unwrap();
            target_file.write_all(&source_bytes).unwrap();
            let user_offset = 34 * LOGICAL_SECTOR_BYTES;
            let data_cluster_offset = 34 * LOGICAL_SECTOR_BYTES;
            let plaintext = vec![0xa5; BIS_DATA_UNIT_BYTES];
            write_user_plain_to_fixture(
                &mut target_file,
                user_offset,
                192 * LOGICAL_SECTOR_BYTES,
                key,
                data_cluster_offset,
                &plaintext,
            )
            .unwrap();
            target_file.sync_all().unwrap();

            let resize = build_fixture_resize_plan(&mut target_file, 10_000, key).unwrap();
            assert_eq!(resize.relocations.len(), 1);
            assert_eq!(resize.expansion.target_user_sectors, 9_920);
            let relocation = resize.relocations[0];
            let old_cipher_offset = user_offset + relocation.source_sector * LOGICAL_SECTOR_BYTES;
            let new_cipher_offset = user_offset + relocation.target_sector * LOGICAL_SECTOR_BYTES;
            let mut old_cipher = vec![0_u8; BIS_DATA_UNIT_BYTES];
            target_file
                .seek(SeekFrom::Start(old_cipher_offset))
                .unwrap();
            target_file.read_exact(&mut old_cipher).unwrap();

            let mut observer = ContinueRestoreDevice;
            let report =
                run_device_resize_phases(&mut target_file, &resize, key, 0, &mut observer).unwrap();
            assert_eq!(report.status, RestoreDeviceStatus::Completed);
            let mut recovered = vec![0_u8; BIS_DATA_UNIT_BYTES];
            read_user_plain_from_fixture(
                &mut target_file,
                user_offset,
                resize.target_user_byte_len,
                key,
                relocation.target_sector * LOGICAL_SECTOR_BYTES,
                &mut recovered,
            )
            .unwrap();
            assert_eq!(recovered, plaintext);
            let mut new_cipher = vec![0_u8; BIS_DATA_UNIT_BYTES];
            target_file
                .seek(SeekFrom::Start(new_cipher_offset))
                .unwrap();
            target_file.read_exact(&mut new_cipher).unwrap();
            assert_ne!(new_cipher, old_cipher);
        }
        let inspection = inspect_nand_image(target.path()).unwrap();
        assert_eq!(inspection.user_partition.last_lba, 9_953);
    }

    #[test]
    fn durable_resize_rejects_a_wrong_key_before_writing_the_fixture() {
        let source = raw_nand_fixture();
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let wrong_keyset = parse_bis_keyset(
            "bis_key_02 = 0000000000000000000000000000000000000000000000000000000000000000",
        )
        .unwrap();

        assert!(matches!(
            restore_and_expand_user_to_fixture(
                source.path(),
                target.path(),
                wrong_keyset.user_key().unwrap(),
                None,
            ),
            Err(RestoreResizeFixtureError::UserVerification(_))
        ));
        assert!(fs::read(target.path())
            .unwrap()
            .iter()
            .all(|byte| *byte == 0));
    }

    #[test]
    fn durable_resize_uses_raw_nand_address_space_for_full_nand() {
        let source = full_nand_fixture();
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();

        assert_eq!(
            restore_and_expand_user_to_fixture(
                source.path(),
                target.path(),
                keyset.user_key().unwrap(),
                None,
            )
            .unwrap(),
            SyntheticRestoreStatus::Completed
        );
        assert_eq!(
            inspect_nand_image(target.path())
                .unwrap()
                .user_partition
                .last_lba,
            9_953
        );
    }

    fn fixture_boot_sector(
        path: &Path,
        key: &BisKey,
        offset: u64,
    ) -> [u8; LOGICAL_SECTOR_BYTES as usize] {
        let mut file = File::open(path).unwrap();
        let mut boot = [0_u8; LOGICAL_SECTOR_BYTES as usize];
        read_user_plain_from_fixture(
            &mut file,
            34 * LOGICAL_SECTOR_BYTES,
            9_920 * LOGICAL_SECTOR_BYTES,
            key,
            offset,
            &mut boot,
        )
        .unwrap();
        boot
    }

    #[test]
    fn partial_fixture_writes_before_primary_boot_keep_the_old_active_geometry() {
        let source = raw_nand_fixture();
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();

        // The synthetic image has one relocation, then FAT (two writes), two
        // GPT copies (four writes), a second relocation-unit rewrite caused
        // by the overlapping FAT mirror, and the backup boot sector: nine writes
        // before the primary boot-sector commit.
        for operation in 1..=9 {
            let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
            let result = with_fixture_fault(
                FixtureFault::PartialWrite {
                    operation,
                    byte_len: 64,
                },
                || restore_and_expand_user_to_fixture(source.path(), target.path(), key, None),
            );
            assert!(matches!(
                result,
                Err(RestoreResizeFixtureError::WriteFailed)
            ));
            let primary_boot = fixture_boot_sector(target.path(), key, 0);
            assert_eq!(
                parse_fat32_boot_sector(&primary_boot, 192)
                    .unwrap()
                    .total_sectors,
                192,
                "write {operation} changed the active primary boot sector"
            );
        }
    }

    #[test]
    fn partial_primary_boot_write_leaves_a_verified_backup_boot_for_manual_recovery() {
        let source = raw_nand_fixture();
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();

        let result = with_fixture_fault(
            FixtureFault::PartialWrite {
                operation: 10,
                byte_len: 64,
            },
            || restore_and_expand_user_to_fixture(source.path(), target.path(), key, None),
        );
        assert!(matches!(
            result,
            Err(RestoreResizeFixtureError::WriteFailed)
        ));

        let backup_boot = fixture_boot_sector(target.path(), key, 6 * LOGICAL_SECTOR_BYTES);
        assert_eq!(
            parse_fat32_boot_sector(&backup_boot, 9_920)
                .unwrap()
                .total_sectors,
            9_920
        );
    }

    #[test]
    fn cache_sync_failure_stops_before_the_next_phase_or_requires_manual_verification() {
        let source = raw_nand_fixture();
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();

        for operation in 1..=5 {
            let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
            let result = with_fixture_fault(FixtureFault::Sync { operation }, || {
                restore_and_expand_user_to_fixture(source.path(), target.path(), key, None)
            });
            assert!(matches!(
                result,
                Err(RestoreResizeFixtureError::WriteFailed)
            ));
            let primary_boot = fixture_boot_sector(target.path(), key, 0);
            assert_eq!(
                parse_fat32_boot_sector(&primary_boot, 192)
                    .unwrap()
                    .total_sectors,
                192
            );
        }

        // The sixth sync follows the primary boot-sector write. Its outcome is
        // unknown to the caller, so it must fail rather than report success;
        // the already verified backup boot sector describes how to recover.
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let result = with_fixture_fault(FixtureFault::Sync { operation: 6 }, || {
            restore_and_expand_user_to_fixture(source.path(), target.path(), key, None)
        });
        assert!(matches!(
            result,
            Err(RestoreResizeFixtureError::WriteFailed)
        ));
        let backup_boot = fixture_boot_sector(target.path(), key, 6 * LOGICAL_SECTOR_BYTES);
        assert_eq!(
            parse_fat32_boot_sector(&backup_boot, 9_920)
                .unwrap()
                .total_sectors,
            9_920
        );
    }

    #[test]
    fn durable_resize_fixture_fault_checkpoints_preserve_the_active_primary_boot_sector() {
        let source = raw_nand_fixture();
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let checkpoints = [
            RestoreAndExpandCheckpoint::BeforeClusterRelocation,
            RestoreAndExpandCheckpoint::BeforeFatMirrors,
            RestoreAndExpandCheckpoint::BeforeBackupGpt,
            RestoreAndExpandCheckpoint::BeforePrimaryGpt,
            RestoreAndExpandCheckpoint::BeforeBackupBootSector,
            RestoreAndExpandCheckpoint::BeforePrimaryBootSector,
        ];

        for checkpoint in checkpoints {
            let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
            let status = restore_and_expand_user_to_fixture(
                source.path(),
                target.path(),
                keyset.user_key().unwrap(),
                Some(checkpoint),
            )
            .unwrap();
            assert_eq!(
                status,
                SyntheticRestoreStatus::InterruptedBefore(checkpoint)
            );

            // The primary FAT32 boot sector is the active geometry until the
            // final commit. A checkpoint must never replace it.
            let mut target_file = File::open(target.path()).unwrap();
            let mut primary_boot = [0_u8; LOGICAL_SECTOR_BYTES as usize];
            read_user_plain_from_fixture(
                &mut target_file,
                34 * LOGICAL_SECTOR_BYTES,
                9_920 * LOGICAL_SECTOR_BYTES,
                keyset.user_key().unwrap(),
                0,
                &mut primary_boot,
            )
            .unwrap();
            assert_eq!(
                parse_fat32_boot_sector(&primary_boot, 192)
                    .unwrap()
                    .total_sectors,
                192
            );

            let inspection = inspect_nand_image(target.path()).unwrap();
            let expected_last_lba = match checkpoint {
                RestoreAndExpandCheckpoint::BeforeClusterRelocation
                | RestoreAndExpandCheckpoint::BeforeFatMirrors
                | RestoreAndExpandCheckpoint::BeforeBackupGpt
                | RestoreAndExpandCheckpoint::BeforePrimaryGpt => 225,
                RestoreAndExpandCheckpoint::BeforeBackupBootSector
                | RestoreAndExpandCheckpoint::BeforePrimaryBootSector => 9_953,
            };
            assert_eq!(inspection.user_partition.last_lba, expected_last_lba);
        }
    }

    #[test]
    fn recovery_report_confirms_a_completed_fixture_without_mutating_it() {
        let source = raw_nand_fixture();
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();

        restore_and_expand_user_to_fixture(source.path(), target.path(), key, None).unwrap();
        let before = fs::read(target.path()).unwrap();
        let report = inspect_restore_resize_fixture_recovery(target.path(), key).unwrap();
        let after = fs::read(target.path()).unwrap();

        assert_eq!(before, after);
        assert_eq!(report.disposition, FixtureRecoveryDisposition::Committed);
        assert_eq!(
            report.primary_gpt,
            FixtureRecoveryGptCopy::Valid {
                user_first_lba: 34,
                user_last_lba: 9_953,
            }
        );
        assert_eq!(
            report.primary_boot,
            FixtureRecoveryBootCopy::Valid {
                user_first_lba: 34,
                user_sectors: 9_920,
                fat_mirrors_match: true,
            }
        );
    }

    #[test]
    fn restart_before_each_sync_discards_volatile_phase_data_and_reports_old_geometry() {
        let source = raw_nand_fixture();
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();

        for operation in 1..=6 {
            let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
            let result = with_fixture_fault(FixtureFault::RestartBeforeSync { operation }, || {
                restore_and_expand_user_to_fixture(source.path(), target.path(), key, None)
            });
            assert!(matches!(result, Err(RestoreResizeFixtureError::Restarted)));

            let report = inspect_restore_resize_fixture_recovery(target.path(), key).unwrap();
            assert_eq!(
                report.disposition,
                FixtureRecoveryDisposition::PreviousGeometryActive,
                "restart before sync {operation}"
            );
            assert!(
                matches!(
                    report.primary_boot,
                    FixtureRecoveryBootCopy::Valid {
                        user_first_lba: 34,
                        user_sectors: 192,
                        ..
                    }
                ),
                "restart before sync {operation} changed primary geometry: {:?}",
                report.primary_boot
            );
        }
    }

    #[test]
    fn recovery_report_finds_backup_boot_after_a_partial_primary_boot_write() {
        let source = raw_nand_fixture();
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();

        let result = with_fixture_fault(
            FixtureFault::PartialWrite {
                operation: 10,
                // The rewritten BPB changes fields on both sides of byte 32.
                // A torn 32-byte encrypted-unit write cannot describe either
                // complete geometry, unlike a coincidentally self-consistent
                // 64-byte prefix.
                byte_len: 32,
            },
            || restore_and_expand_user_to_fixture(source.path(), target.path(), key, None),
        );
        assert!(matches!(
            result,
            Err(RestoreResizeFixtureError::WriteFailed)
        ));

        let report = inspect_restore_resize_fixture_recovery(target.path(), key).unwrap();
        assert_eq!(
            report.disposition,
            FixtureRecoveryDisposition::ManualRecoveryFromBackupBoot
        );
        assert_eq!(report.primary_boot, FixtureRecoveryBootCopy::Invalid);
        assert_eq!(
            report.backup_boot,
            FixtureRecoveryBootCopy::Valid {
                user_first_lba: 34,
                user_sectors: 9_920,
                fat_mirrors_match: true,
            }
        );
    }

    #[test]
    fn planning_marks_restore_without_boot_files_as_writer_eligible() {
        let plan = create_operation_plan(
            OperationMode::Restore,
            ArtifactReceipt {
                id: "backup".into(),
                kind: ArtifactKind::Backup,
                byte_len: 1,
            },
            None,
            None,
            None,
            BlockDevice {
                path: "/dev/test".into(),
                model: "test".into(),
                byte_len: 2,
                is_removable: true,
                is_read_only: false,
                boot_partitions_available: false,
            },
        )
        .unwrap();
        assert!(plan.requires_read_only_preflight);
        assert_eq!(plan.write_operations_enabled, cfg!(target_os = "linux"));
    }

    #[test]
    fn device_resize_phases_commit_and_verify_on_a_fixture() {
        let source = raw_nand_fixture();
        let target = restore_fixture(10_000 * LOGICAL_SECTOR_BYTES);
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();
        let mut copy_observer = ContinueRestoreFixture;
        restore_raw_nand_to_fixture(source.path(), target.path(), &mut copy_observer).unwrap();

        let mut target_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(target.path())
            .unwrap();
        let plan = build_fixture_resize_plan(&mut target_file, 10_000, key).unwrap();
        let mut observer = RecordedDeviceProgress {
            progress: Vec::new(),
            cancel_before: None,
        };
        let report = run_device_resize_phases(
            &mut target_file,
            &plan,
            key,
            source.as_file().metadata().unwrap().len(),
            &mut observer,
        )
        .unwrap();
        assert_eq!(report.status, RestoreDeviceStatus::Completed);
        assert!(observer
            .progress
            .iter()
            .any(|progress| progress.phase == RestoreDevicePhase::CommittingPrimaryBootSector));
        drop(target_file);
        assert_eq!(
            inspect_restore_resize_fixture_recovery(target.path(), key)
                .unwrap()
                .disposition,
            FixtureRecoveryDisposition::Committed
        );
    }

    #[test]
    fn complete_resize_plan_can_be_built_from_source_before_target_write() {
        let source_file = raw_nand_fixture();
        let source = ImageReader::open(source_file.path()).unwrap();
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();
        let source_plan = build_resize_plan_from_source(&source, 0, 10_000, key).unwrap();
        let mut file = File::open(source_file.path()).unwrap();
        let file_plan = build_fixture_resize_plan(&mut file, 10_000, key).unwrap();
        assert_eq!(source_plan.expansion, file_plan.expansion);
        assert_eq!(source_plan.gpt.user_first_lba, file_plan.gpt.user_first_lba);
        assert_eq!(source_plan.relocations, file_plan.relocations);
        assert_eq!(source_plan.primary_table, file_plan.primary_table);
        assert_eq!(source_plan.expanded_fat, file_plan.expanded_fat);
    }

    #[test]
    fn resize_read_error_reports_offset_without_exposing_data() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut reader = File::open(file.path()).unwrap();
        let mut sector = [0_u8; 512];
        let error = read_fixture_at(&mut reader, 512, &mut sector).unwrap_err();
        assert!(matches!(
            error,
            RestoreResizeFixtureError::ReadFailed {
                offset: 512,
                length: 512,
                step: "read",
                ..
            }
        ));
        assert_eq!(error.to_string(), "Nie udało się odczytać danych celu.");
    }

    #[test]
    fn sector_aligned_read_returns_only_requested_gpt_table_bytes() {
        let mut file = tempfile::tempfile().unwrap();
        let bytes: Vec<u8> = (0..5 * LOGICAL_SECTOR_BYTES as usize)
            .map(|index| (index % 251) as u8)
            .collect();
        file.write_all(&bytes).unwrap();
        let mut table = [0_u8; 1408];
        read_fixture_at(&mut file, 1024, &mut table).unwrap();
        assert_eq!(&table[..], &bytes[1024..2432]);
        read_fixture_at(&mut file, 1025, &mut table).unwrap();
        assert_eq!(&table[..], &bytes[1025..2433]);
    }

    #[test]
    fn allows_raw_restore_without_a_keyset_but_not_resize() {
        let backup = ArtifactReceipt {
            id: "backup".into(),
            kind: ArtifactKind::Backup,
            byte_len: 1,
        };
        let target = BlockDevice {
            path: "/dev/test".into(),
            model: "test".into(),
            byte_len: 2,
            is_removable: true,
            is_read_only: false,
            boot_partitions_available: false,
        };
        assert!(create_operation_plan(
            OperationMode::Restore,
            backup.clone(),
            None,
            None,
            None,
            target.clone()
        )
        .is_ok());
        assert_eq!(
            create_operation_plan(
                OperationMode::RestoreAndExpandUser,
                backup,
                None,
                None,
                None,
                target,
            ),
            Err(OperationPlanError::KeysetRequired)
        );
    }

    #[test]
    fn inspects_a_valid_raw_nand_gpt_without_touching_a_target() {
        let image = raw_nand_fixture();
        let inspection = inspect_raw_nand_image(image.path()).unwrap();
        assert_eq!(inspection.backup_gpt_lba, 255);
        assert_eq!(inspection.user_partition.name, "USER");
        assert_eq!(inspection.user_partition.first_lba, 34);
        assert_eq!(inspection.user_partition.last_lba, 225);
    }

    #[test]
    fn recognises_full_nand_only_at_the_documented_boot_area_offset() {
        let image = full_nand_fixture();
        let inspection = inspect_nand_image(image.path()).unwrap();
        assert_eq!(inspection.format, NandImageFormat::FullNand);
        assert_eq!(inspection.raw_nand_offset_bytes, FULL_NAND_BOOT_AREA_BYTES);
        assert_eq!(
            inspection.container_byte_len,
            image.as_file().metadata().unwrap().len()
        );
        assert_eq!(inspection.image_byte_len, 256 * LOGICAL_SECTOR_BYTES);
        assert_eq!(inspection.boot0_source, BootComponentSource::Embedded);
        assert_eq!(inspection.boot1_source, BootComponentSource::Embedded);
        assert_eq!(inspection.user_partition.name, "USER");
    }

    #[test]
    fn full_nand_resize_uses_the_raw_nand_address_space() {
        let image = full_nand_fixture();
        let keyset = user_keyset_fixture();
        let target = BlockDevice {
            path: "/dev/test".into(),
            model: "test".into(),
            byte_len: 10_000 * LOGICAL_SECTOR_BYTES,
            is_removable: true,
            is_read_only: false,
            boot_partitions_available: true,
        };
        let plan = create_operation_plan(
            OperationMode::RestoreAndExpandUser,
            ArtifactReceipt {
                id: "backup".into(),
                kind: ArtifactKind::Backup,
                byte_len: 1,
            },
            Some(ArtifactReceipt {
                id: "keyset".into(),
                kind: ArtifactKind::Keyset,
                byte_len: 1,
            }),
            None,
            None,
            target,
        )
        .unwrap();
        let report =
            preflight_nand_restore(plan, image.path(), Some(keyset.path()), None, None).unwrap();
        assert_eq!(report.source.format, NandImageFormat::FullNand);
        assert_eq!(report.expanded_user.unwrap().target_user_sectors, 9_920);
        assert!(report.keyset_validated_against_user_fat32);
    }

    #[test]
    fn raw_nand_accepts_only_an_exact_complete_boot_pair() {
        let boot0 = boot_fixture(BOOT_PARTITION_BYTES);
        let boot1 = boot_fixture(BOOT_PARTITION_BYTES);
        assert_eq!(
            validate_boot_backups(
                NandImageFormat::RawNand,
                Some(boot0.path()),
                Some(boot1.path())
            ),
            Ok((BootComponentSource::Supplied, BootComponentSource::Supplied))
        );
        assert_eq!(
            validate_boot_backups(NandImageFormat::RawNand, Some(boot0.path()), None),
            Err(BootBackupError::IncompletePair)
        );
        let too_small = boot_fixture(BOOT_PARTITION_BYTES - 1);
        assert_eq!(
            validate_boot_backups(
                NandImageFormat::RawNand,
                Some(boot0.path()),
                Some(too_small.path())
            ),
            Err(BootBackupError::IncorrectSize)
        );
        assert_eq!(
            validate_boot_backups(
                NandImageFormat::FullNand,
                Some(boot0.path()),
                Some(boot1.path())
            ),
            Err(BootBackupError::UnexpectedForFullNand)
        );
    }

    #[test]
    fn raw_nand_preflight_reports_a_supplied_boot_pair() {
        let image = raw_nand_fixture();
        let boot0 = boot_fixture(BOOT_PARTITION_BYTES);
        let boot1 = boot_fixture(BOOT_PARTITION_BYTES);
        let plan = create_operation_plan(
            OperationMode::Restore,
            ArtifactReceipt {
                id: "backup".into(),
                kind: ArtifactKind::Backup,
                byte_len: 1,
            },
            None,
            Some(ArtifactReceipt {
                id: "boot0".into(),
                kind: ArtifactKind::Boot0,
                byte_len: BOOT_PARTITION_BYTES,
            }),
            Some(ArtifactReceipt {
                id: "boot1".into(),
                kind: ArtifactKind::Boot1,
                byte_len: BOOT_PARTITION_BYTES,
            }),
            BlockDevice {
                path: "/dev/test".into(),
                model: "test".into(),
                byte_len: 1_000_000,
                is_removable: true,
                is_read_only: false,
                boot_partitions_available: true,
            },
        )
        .unwrap();
        let report = preflight_nand_restore(
            plan,
            image.path(),
            None,
            Some(boot0.path()),
            Some(boot1.path()),
        )
        .unwrap();
        assert_eq!(report.source.boot0_source, BootComponentSource::Supplied);
        assert_eq!(report.source.boot1_source, BootComponentSource::Supplied);
    }

    #[test]
    fn rejects_a_gpt_with_a_corrupt_partition_table() {
        let mut image = raw_nand_fixture();
        image
            .as_file_mut()
            .seek(SeekFrom::Start(2 * LOGICAL_SECTOR_BYTES))
            .unwrap();
        image.as_file_mut().write_all(&[0xFF]).unwrap();
        image.as_file_mut().flush().unwrap();
        assert!(matches!(
            inspect_raw_nand_image(image.path()),
            Err(InspectionError::InvalidGpt(
                "niepoprawne CRC tablicy partycji"
            ))
        ));
    }

    #[test]
    fn preflight_calculates_the_expanded_user_from_the_target() {
        let image = raw_nand_fixture();
        let keyset = user_keyset_fixture();
        let report = preflight_raw_nand_restore(
            create_operation_plan(
                OperationMode::RestoreAndExpandUser,
                ArtifactReceipt {
                    id: "backup".into(),
                    kind: ArtifactKind::Backup,
                    byte_len: 1,
                },
                Some(ArtifactReceipt {
                    id: "keyset".into(),
                    kind: ArtifactKind::Keyset,
                    byte_len: 1,
                }),
                None,
                None,
                BlockDevice {
                    path: "/dev/test".into(),
                    model: "test".into(),
                    byte_len: 10_000 * LOGICAL_SECTOR_BYTES,
                    is_removable: true,
                    is_read_only: false,
                    boot_partitions_available: false,
                },
            )
            .unwrap(),
            image.path(),
            Some(keyset.path()),
        )
        .unwrap();
        assert_eq!(report.expanded_user.unwrap().target_user_sectors, 9_920);
        assert!(report.keyset_validated_against_user_fat32);
        let fat32 = report.user_fat32.unwrap();
        assert_eq!(fat32.cluster_count, 4);
        assert_eq!(fat32.allocated_clusters, 1);
        assert_eq!(fat32.free_clusters, 3);
        assert_eq!(fat32.bad_clusters, 0);
        assert_eq!(fat32.chain_count, 1);
        assert_eq!(fat32.largest_chain_clusters, 1);
        let fat32_expansion = report.fat32_expansion.unwrap();
        assert_eq!(fat32_expansion.target_sectors_per_fat, 3);
        assert_eq!(fat32_expansion.data_start_shift_sectors, 4);
        assert_eq!(fat32_expansion.target_cluster_count, 308);
    }

    #[test]
    fn parses_complete_fat_chains_and_orders_relocations_backwards() {
        let (boot_sector, fat) = fat32_chain_fixture();
        let chains = parse_fat32_chains(&boot_sector, &fat).unwrap();
        assert_eq!(chains.len(), 2);
        assert_eq!(chains[0].clusters, vec![2, 3]);
        assert_eq!(chains[1].clusters, vec![4]);

        let expansion = plan_fat32_expansion(&boot_sector, 9_920).unwrap();
        let moves = plan_fat32_cluster_relocations(&boot_sector, &fat, &expansion).unwrap();
        assert_eq!(
            moves.iter().map(|move_| move_.cluster).collect::<Vec<_>>(),
            vec![4, 3, 2]
        );
        assert_eq!(moves[0].source_sector, 98);
        assert_eq!(moves[0].target_sector, 102);
        assert_eq!(moves[2].source_sector, 34);
        assert_eq!(moves[2].target_sector, 38);
    }

    #[test]
    fn rejects_cross_links_and_cycles_in_fat_chains() {
        let (boot_sector, mut cross_link) = fat32_chain_fixture();
        put_u32(&mut cross_link, 8, 4);
        put_u32(&mut cross_link, 12, 4);
        assert!(matches!(
            parse_fat32_chains(&boot_sector, &cross_link),
            Err(UserFilesystemError::InvalidBootSector(
                "cross-link w łańcuchu FAT"
            ))
        ));

        let (_, mut cycle) = fat32_chain_fixture();
        put_u32(&mut cycle, 8, 3);
        put_u32(&mut cycle, 12, 2);
        put_u32(&mut cycle, 16, 0);
        assert!(matches!(
            parse_fat32_chains(&boot_sector, &cycle),
            Err(UserFilesystemError::InvalidBootSector(
                "cykl w łańcuchu FAT"
            ))
        ));
    }

    #[test]
    fn prepares_expanded_fat_and_boot_metadata_without_mutating_source() {
        let (boot_sector, fat) = fat32_chain_fixture();
        let expansion = plan_fat32_expansion(&boot_sector, 9_920).unwrap();
        let expanded_fat = build_expanded_fat(&fat, &expansion).unwrap();
        assert_eq!(&expanded_fat[..fat.len()], fat.as_slice());
        assert!(expanded_fat[fat.len()..].iter().all(|byte| *byte == 0));

        let original_sector = fat32_boot_sector_bytes(&boot_sector);
        let rewritten = rewrite_fat32_boot_sector(&original_sector, &expansion).unwrap();
        assert_eq!(&rewritten[510..512], &[0x55, 0xaa]);
        assert_eq!(&rewritten[19..21], &[0, 0]);
        assert_eq!(
            parse_fat32_boot_sector(&rewritten, u64::from(expansion.target_user_sectors))
                .unwrap()
                .sectors_per_fat,
            expansion.target_sectors_per_fat
        );
        assert_eq!(
            original_sector[36..40],
            boot_sector.sectors_per_fat.to_le_bytes()
        );
    }

    #[test]
    fn synthetic_restore_and_resize_relocates_data_and_updates_both_gpts() {
        let image = raw_nand_fixture();
        let mut source = fs::read(image.path()).unwrap();
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();
        let payload = (0..BIS_DATA_UNIT_BYTES)
            .map(|index| ((index * 29 + 7) % 256) as u8)
            .collect::<Vec<_>>();
        write_user_plain(
            &mut source,
            34 * LOGICAL_SECTOR_BYTES,
            192 * LOGICAL_SECTOR_BYTES,
            key,
            34 * LOGICAL_SECTOR_BYTES,
            &payload,
        )
        .unwrap();

        let outcome = restore_and_expand_user_image(&source, 10_000, key, None).unwrap();
        assert_eq!(outcome.status, SyntheticRestoreStatus::Completed);
        assert_eq!(
            outcome.raw_nand.len(),
            10_000 * LOGICAL_SECTOR_BYTES as usize
        );

        let gpt = parse_resize_gpt(&outcome.raw_nand).unwrap();
        assert_eq!(gpt.user_first_lba, 34);
        assert_eq!(gpt.user_last_lba, 9_953);
        assert_eq!(read_u64(&gpt.header, 32).unwrap(), 9_999);
        let backup_header_offset = 9_999 * LOGICAL_SECTOR_BYTES as usize;
        let backup_header: [u8; 512] = outcome.raw_nand
            [backup_header_offset..backup_header_offset + 512]
            .try_into()
            .unwrap();
        assert_eq!(&backup_header[..8], b"EFI PART");
        assert_eq!(read_u64(&backup_header, 24).unwrap(), 9_999);
        assert_eq!(read_u64(&backup_header, 32).unwrap(), 1);
        assert_eq!(
            read_u32(&backup_header, 16).unwrap(),
            crc32_with_zeroed_field(&backup_header[..GPT_MIN_HEADER_SIZE], 16, 4)
        );
        let backup_table_lba = read_u64(&backup_header, 72).unwrap() as usize;
        assert_eq!(
            &outcome.raw_nand[backup_table_lba * 512..backup_table_lba * 512 + gpt.table.len()],
            gpt.table.as_slice()
        );

        let mut moved = vec![0_u8; BIS_DATA_UNIT_BYTES];
        read_user_plain(
            &outcome.raw_nand,
            34 * LOGICAL_SECTOR_BYTES,
            9_920 * LOGICAL_SECTOR_BYTES,
            key,
            38 * LOGICAL_SECTOR_BYTES,
            &mut moved,
        )
        .unwrap();
        assert_eq!(moved, payload);

        let mut primary_boot = [0_u8; 512];
        let mut backup_boot = [0_u8; 512];
        read_user_plain(
            &outcome.raw_nand,
            34 * LOGICAL_SECTOR_BYTES,
            9_920 * LOGICAL_SECTOR_BYTES,
            key,
            0,
            &mut primary_boot,
        )
        .unwrap();
        read_user_plain(
            &outcome.raw_nand,
            34 * LOGICAL_SECTOR_BYTES,
            9_920 * LOGICAL_SECTOR_BYTES,
            key,
            6 * LOGICAL_SECTOR_BYTES,
            &mut backup_boot,
        )
        .unwrap();
        assert_eq!(primary_boot, backup_boot);
        assert_eq!(
            parse_fat32_boot_sector(&primary_boot, 9_920)
                .unwrap()
                .sectors_per_fat,
            3
        );
        let output = tempfile::NamedTempFile::new().unwrap();
        fs::write(output.path(), &outcome.raw_nand).unwrap();
        let expanded_user = validate_user_fat32(
            output.path(),
            0,
            &GptPartition {
                name: "USER".into(),
                first_lba: 34,
                last_lba: 9_953,
            },
            key,
        )
        .unwrap();
        assert_eq!(expanded_user.cluster_count, 308);
        assert_eq!(expanded_user.allocated_clusters, 1);
    }

    #[test]
    fn synthetic_restore_interruptions_keep_the_old_layout_readable() {
        let image = raw_nand_fixture();
        let source = fs::read(image.path()).unwrap();
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let key = keyset.user_key().unwrap();
        let checkpoints = [
            RestoreAndExpandCheckpoint::BeforeClusterRelocation,
            RestoreAndExpandCheckpoint::BeforeFatMirrors,
            RestoreAndExpandCheckpoint::BeforeBackupGpt,
            RestoreAndExpandCheckpoint::BeforePrimaryGpt,
            RestoreAndExpandCheckpoint::BeforeBackupBootSector,
            RestoreAndExpandCheckpoint::BeforePrimaryBootSector,
        ];

        for checkpoint in checkpoints {
            let outcome =
                restore_and_expand_user_image(&source, 10_000, key, Some(checkpoint)).unwrap();
            assert_eq!(
                outcome.status,
                SyntheticRestoreStatus::InterruptedBefore(checkpoint)
            );
            assert_eq!(&outcome.raw_nand[..source.len()], source.as_slice());
            let output = tempfile::NamedTempFile::new().unwrap();
            fs::write(output.path(), &outcome.raw_nand).unwrap();
            assert_eq!(
                inspect_raw_nand_image(output.path())
                    .unwrap()
                    .backup_gpt_lba,
                255
            );
            let inspection = validate_user_fat32(
                output.path(),
                0,
                &GptPartition {
                    name: "USER".into(),
                    first_lba: 34,
                    last_lba: 225,
                },
                key,
            )
            .unwrap();
            assert_eq!(inspection.boot_sector.total_sectors, 192);
        }
    }

    #[test]
    fn synthetic_restore_uses_raw_nand_address_space_for_full_nand() {
        let image = full_nand_fixture();
        let source = fs::read(image.path()).unwrap();
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let outcome =
            restore_and_expand_user_image(&source, 10_000, keyset.user_key().unwrap(), None)
                .unwrap();

        assert_eq!(outcome.status, SyntheticRestoreStatus::Completed);
        assert_eq!(
            outcome.raw_nand.len(),
            10_000 * LOGICAL_SECTOR_BYTES as usize
        );
        assert_eq!(
            parse_resize_gpt(&outcome.raw_nand).unwrap().user_last_lba,
            9_953
        );
    }

    #[test]
    fn resize_preflight_rejects_a_parseable_keyset_that_cannot_decrypt_user_fat32() {
        let image = raw_nand_fixture();
        let mut wrong_keyset = tempfile::NamedTempFile::new().unwrap();
        wrong_keyset
            .write_all(
                b"bis_key_02 = 00112233445566778899aabbccddeeff0f0e0d0c0b0a09080706050403020100",
            )
            .unwrap();
        wrong_keyset.flush().unwrap();
        let plan = create_operation_plan(
            OperationMode::RestoreAndExpandUser,
            ArtifactReceipt {
                id: "backup".into(),
                kind: ArtifactKind::Backup,
                byte_len: 1,
            },
            Some(ArtifactReceipt {
                id: "keyset".into(),
                kind: ArtifactKind::Keyset,
                byte_len: 1,
            }),
            None,
            None,
            BlockDevice {
                path: "/dev/test".into(),
                model: "test".into(),
                byte_len: 10_000 * LOGICAL_SECTOR_BYTES,
                is_removable: true,
                is_read_only: false,
                boot_partitions_available: false,
            },
        )
        .unwrap();

        assert!(matches!(
            preflight_raw_nand_restore(plan, image.path(), Some(wrong_keyset.path())),
            Err(PreflightError::UserFilesystem(
                UserFilesystemError::InvalidBootSector(_)
            ))
        ));
    }

    #[test]
    fn rejects_user_with_different_fat_mirrors() {
        let mut image = raw_nand_fixture();
        let mirrored_fat_ciphertext_offset = 34 * LOGICAL_SECTOR_BYTES as usize
            + BIS_DATA_UNIT_BYTES
            + LOGICAL_SECTOR_BYTES as usize;
        image
            .as_file_mut()
            .seek(SeekFrom::Start(mirrored_fat_ciphertext_offset as u64))
            .unwrap();
        image.as_file_mut().write_all(&[0xff]).unwrap();
        image.as_file_mut().flush().unwrap();

        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let user_partition = GptPartition {
            name: "USER".into(),
            first_lba: 34,
            last_lba: 225,
        };
        assert!(matches!(
            validate_user_fat32(image.path(), 0, &user_partition, keyset.user_key().unwrap()),
            Err(UserFilesystemError::InvalidBootSector(
                "kopie FAT różnią się"
            ))
        ));
    }

    #[test]
    fn reads_primary_gpt_across_split_raw_nand_parts() {
        let image = raw_nand_fixture();
        let bytes = fs::read(image.path()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("rawnand.bin.00");
        let second = directory.path().join("rawnand.bin.01");
        fs::write(&first, &bytes[..1100]).unwrap();
        fs::write(&second, &bytes[1100..]).unwrap();

        let inspection = inspect_raw_nand_image(&first).unwrap();
        assert_eq!(inspection.source_part_count, 2);
        assert_eq!(inspection.image_byte_len, bytes.len() as u64);
        assert_eq!(inspection.user_partition.name, "USER");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn reads_split_backup_from_long_unicode_windows_path() {
        let image = raw_nand_fixture();
        let bytes = fs::read(image.path()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let long_root = if root.to_string_lossy().starts_with(r"\\?\") {
            root
        } else {
            PathBuf::from(format!(r"\\?\{}", root.display()))
        };
        let nested = long_root
            .join("żółć_日本語".repeat(10))
            .join("długi_katalog".repeat(10));
        fs::create_dir_all(&nested).unwrap();
        let first = nested.join("pełny_backup.bin.00");
        let second = nested.join("pełny_backup.bin.01");
        assert!(first.to_string_lossy().len() > 260);
        fs::write(&first, &bytes[..1100]).unwrap();
        fs::write(&second, &bytes[1100..]).unwrap();
        let inspection = inspect_raw_nand_image(&first).unwrap();
        assert_eq!(inspection.source_part_count, 2);
        assert_eq!(inspection.image_byte_len, bytes.len() as u64);
    }

    #[test]
    fn rejects_a_split_backup_with_a_gap() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("rawnand.bin.00");
        let third = directory.path().join("rawnand.bin.02");
        fs::write(&first, [1_u8]).unwrap();
        fs::write(&third, [1_u8]).unwrap();
        assert!(matches!(
            inspect_raw_nand_image(&first),
            Err(InspectionError::MissingSplitPart)
        ));
    }

    #[test]
    fn rejects_a_non_regular_backup_source() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            inspect_nand_image(directory.path()),
            Err(InspectionError::ImageNotRegularFile)
        ));
    }

    #[test]
    fn parses_lockpick_and_biskeydump_bis_key_formats_without_retaining_text() {
        let combined = parse_bis_keyset(
            "bis_key_02 = 00112233445566778899aabbccddeeff0f0e0d0c0b0a09080706050403020100",
        )
        .unwrap();
        assert!(combined.user_key().is_some());

        let separate = parse_bis_keyset(
            "BIS Key 2 (crypt): 00112233445566778899AABBCCDDEEFF\nBIS Key 2 (tweak): 0F0E0D0C0B0A09080706050403020100",
        )
        .unwrap();
        assert!(separate.user_key().is_some());
    }

    #[test]
    fn rejects_incomplete_or_malformed_user_bis_keys() {
        assert!(matches!(
            parse_bis_keyset("BIS Key 2 (crypt): 00112233445566778899aabbccddeeff"),
            Err(KeysetError::IncompleteBisKey)
        ));
        assert!(matches!(
            parse_bis_keyset("bis_key_02 = not-a-key"),
            Err(KeysetError::InvalidBisKey)
        ));
    }

    #[test]
    fn refuses_an_oversized_keyset_before_reading_its_contents() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.as_file_mut().set_len(MAX_KEYSET_BYTES + 1).unwrap();
        assert!(matches!(
            parse_bis_keyset_file(file.path()),
            Err(KeysetFileError::TooLarge)
        ));
    }

    #[test]
    fn aes_xts_round_trip_is_limited_to_a_16_kib_partition_relative_unit() {
        let keyset = parse_bis_keyset(TEST_USER_KEYSET).unwrap();
        let crypto = SwitchAesXts128::new(keyset.user_key().unwrap());
        let mut data = (0..BIS_DATA_UNIT_BYTES)
            .map(|index| ((index * 17 + 31) % 256) as u8)
            .collect::<Vec<_>>();
        let original = data.clone();

        crypto.encrypt_unit(0x1234, &mut data).unwrap();
        assert_ne!(data, original);
        // Golden vector independently produced with OpenSSL EVP AES-128-XTS.
        // The 16-byte IV contains the partition-relative unit index in the
        // Switch's big-endian tail layout; the asserted prefix spans 4 blocks.
        assert_eq!(
            &data[..64],
            &[
                0x23, 0x06, 0x57, 0xfa, 0x8d, 0x64, 0x88, 0xb0, 0x2a, 0xee, 0xc5, 0x71, 0x61, 0xe9,
                0x61, 0x1b, 0x73, 0x9d, 0xbf, 0xa7, 0xa6, 0x22, 0xf6, 0xfa, 0x61, 0x87, 0xbc, 0x97,
                0xb5, 0x57, 0x58, 0x39, 0x59, 0x90, 0x97, 0x67, 0x79, 0x0d, 0x49, 0x5a, 0x6c, 0xcc,
                0x02, 0x38, 0xd5, 0x4c, 0xce, 0xe3, 0x5c, 0xf6, 0x91, 0xdc, 0xfd, 0x58, 0x95, 0xf5,
                0xee, 0xcc, 0xa9, 0x2c, 0x34, 0x5b, 0x33, 0x88,
            ]
        );
        crypto.decrypt_unit(0x1234, &mut data).unwrap();
        assert_eq!(data, original);
        assert_eq!(
            crypto.encrypt_unit(0, &mut data[..BIS_DATA_UNIT_BYTES - 1]),
            Err(CryptoError::InvalidDataUnitSize)
        );
    }
}
