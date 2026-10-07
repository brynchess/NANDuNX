# W3 — Windows enumeration and read-only preflight

Status on 2026-10-03. As agreed with the user, W3 completed implementation and tests without a physical reader. Base revision: `d28bf98b23ae6a4b0dae918338bd2ab7949c38f6`. Tests used a separate tracked-file export containing the W3 changes and VHDX script, excluding this report. Its SHA-256 was `e9af043a37551198ab514bd42cad1aae9e154f15d4450b79b57027171502ff34`. The server was Windows Server 2022 with MSVC host `x86_64-pc-windows-msvc`, Rust/Cargo 1.90.0, Git 2.56.0.windows.1, Node 24.21.0, and npm 11.19.0.

## Implementation

`nandunx-core` enumerated present disk interfaces through SetupAPI, opened them read-only, and obtained physical disk number, capacity, logical/physical sector sizes, model, serial, PnP instance ID, and GPT disk ID. The public list still returned full `\\.\PhysicalDriveN` paths. The extended snapshot stayed in Rust memory and redacted identifiers in `Debug`.

Preflight mapped every GUID volume to physical extents, including volumes without drive letters. It located all split-backup parts, the keyset, BOOT0/BOOT1, and the executable on disks; it read the Windows system directory and active pagefiles. It rejected changed snapshots, missing serial/PnP/GPT IDs, 4Kn, unknown partition layouts, LDM/dynamic disks, Storage Spaces, RAID, multi-disk volumes, system/pagefile disks, and a source on the target. Read errors or ambiguity failed closed. Restore and in-place modes still used shared GPT/FAT/BIS validation in core.

At W3, Windows authorization, RW access, locks, and writer still returned `UnsupportedPlatform`. The desktop did not enable Windows writes.

## Validation without eMMC

| Check | Result |
| --- | --- |
| Linux `cargo fmt --check`, `cargo test --workspace --locked`, `npm run build` | Passed: 52 core and 6 desktop tests |
| Windows MSVC: `npm.cmd ci`, `npm.cmd run build`, `cargo fmt --check`, `cargo test --workspace --locked`, `cargo build -p nandunx-desktop --locked` | Passed, including negative Win32 core cases and 6 desktop tests |
| Read-only probe of the Windows system disk | Enumeration passed; preflight rejected `MissingIdentity` |
| Disposable 64 MiB GPT VHDX without a drive letter | 512/4096 sector enumeration and extent mapping passed; script detached the VHDX |
| Full VHDX preflight | Expected `MissingIdentity` rejection |

Synthetic tests covered changed serial/GPT ID at the same disk number, missing IDs, 4Kn, source on target, system/pagefile disks, and a volume spanning disks. [w3-vhd-probe.ps1](../scripts/windows/w3-vhd-probe.ps1) created only its own disposable VHDX, verified its disk mapping, and detached it in `finally`. No real NAND or keys were used.

## Gate for W4

There was no physical reader. W4 had to begin with read-only geometry, PnP, serial, and GPT ID checks for the target adapter, plus independent RAWNAND preflight. Identity had to remain stable while distinguishing media swaps in the same reader. Without that guarantee, writes must remain blocked until identity protection improves. Only then could lock, UAC helper, and durable I/O tests proceed on fixtures/VHD. A Windows build and VHDX test alone did not authorize eMMC writes.

Win32 references: [SetupAPI](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupdigetdeviceinterfacedetailw), [volume extents](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_volume_get_volume_disk_extents), and [LDM partition types](https://learn.microsoft.com/en-us/windows-hardware/drivers/storage/msft-partition).
