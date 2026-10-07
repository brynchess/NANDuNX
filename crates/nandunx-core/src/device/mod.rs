//! Platform boundary for physical devices. Fixture I/O stays in the core.

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
pub(crate) mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::{
    current_physical_block, ensure_same_authorized_device, ensure_unmounted_target,
    list_block_devices, mounted_at_for_target, paths_name_same_device,
};
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub(crate) use unsupported::{
    current_physical_block, ensure_same_authorized_device, ensure_unmounted_target,
    list_block_devices, mounted_at_for_target, paths_name_same_device,
};
#[cfg(target_os = "windows")]
pub(crate) use windows::{
    current_physical_block, ensure_same_authorized_device, ensure_unmounted_target,
    list_block_devices, mounted_at_for_target, paths_name_same_device,
};
