//! Other platforms have no physical-device write support.
use crate::{AuthorizedRestoreTarget, BlockDevice, RestoreDeviceError, RestoreTargetSafetyError};
use std::{fs::File, io, path::Path};

pub(crate) fn list_block_devices() -> io::Result<Vec<BlockDevice>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "device enumeration is not implemented",
    ))
}

pub(crate) fn current_physical_block(
    _: &BlockDevice,
) -> Result<(String, BlockDevice), RestoreTargetSafetyError> {
    Err(RestoreTargetSafetyError::UnsupportedPlatform)
}

pub(crate) fn mounted_at_for_target(_: &str) -> io::Result<Vec<String>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "volume mapping is not implemented",
    ))
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
