# NANDuNX on Windows — technical assessment

Assessment begun on 2026-10-01 from repository code and official Microsoft, Tauri, GitHub, and Gitea documentation. [ROADMAP.md](../ROADMAP.md) is the sole source of truth for implementation order and current status. This document describes scope, design decisions, and acceptance gates; it does not declare Windows generally supported.

## Feasibility and first-release scope

The port can share backup parsers, GPT/FAT32, planning, AES-128-XTS with 16 KiB units and BIS Key 2, verification, transaction phases, and the React frontend. The major work is a Win32 replacement for the Linux device-safety barrier and integration of desktop writing with UAC elevation.

The proposed first release targets Windows 11 x64, `x86_64-pc-windows-msvc`, a Tauri GUI with an NSIS `.exe` installer, and all three modes: plain restore, restore+resize, and in-place resize. Windows gets no NANDuNX Web UI, headless package, or service. Tauri's WebView2 renders the local frontend in the app window without a NANDuNX HTTP server. Windows 10, ARM64, MSI, and auto-update are later work. BOOT0/BOOT1 still require separate Hekate restoration. The port adds no driver or key acquisition.

A supported reader must expose the main eMMC area as a Windows physical disk with 512 B logical sectors. The removable flag alone does not establish safety. The initial version excludes 4Kn, Storage Spaces, dynamic disks, RAID, and devices with ambiguous identities or volume dependencies.

## Repository audit at the start of the port

| Area | Linux starting point | Windows work |
| --- | --- | --- |
| Formats and cryptography | UI-independent Rust, synthetic vectors and fault injection | Keep algorithms and run the same tests natively |
| Device list | `list_block_devices` read `/sys/block` and returned `/dev/...` | Win32 enumeration and metadata |
| Target preflight | `current_physical_block`, mountinfo, `/sys/dev/block` | Physical identity and all volume extents, even without drive letters |
| Authorization/write | `AuthorizedRestoreTarget` held an `fs2`-locked `File` | Platform-specific protected session; a file lock is not a disk lock |
| Handle identity | Unix `rdev` comparison; previous non-Unix path did nothing | Required Win32 handle check; no permissive fallback |
| Source paths | Canonical paths/device-node comparisons | Map ordinary files and every split part, keyset, and boot file to underlying disks |
| Desktop | Linux Tauri already ran three modes with session progress/cancellation | Win32 sessions and UAC manifest |
| Web | Axum sessions and `TempDir` uploads | Contract reference only; no Windows systemd port |
| Bundle | `deb` only, existing `icon.ico` | Windows NSIS, WebView2, UAC config |
| Release | Linux Bash, GitHub Ubuntu, Gitea Linux runner | Native Windows build and combined artifact manifest |

The original audit did not itself build or run Windows. A successful user-reported Switch test on Linux hardware did not validate Win32 disk access. Later W1–W5 work and remaining gates are tracked in the roadmap.

## System boundary and permissions

Core owns `device/linux`, `device/windows`, and the common device contract; adapters call public core APIs. Win32 dependencies apply only under `cfg(target_os = "windows")` and Linux `/sys`/`/proc` under `cfg(target_os = "linux")`. Unsupported systems and unimplemented checks fail before writing.

The contract covers enumeration, capacity, logical/physical sector sizes, open-handle identity, volume/source dependencies, authorization, read/write, sync, and protection release. The plan stores a session identity snapshot, not just a path and capacity: `PhysicalDriveN` numbers may be reused after disconnection, and model plus capacity are insufficient. Identifiers remain in memory rather than logs/configuration.

Use SetupAPI disk interfaces and `DeviceIoControl` queries such as `IOCTL_STORAGE_QUERY_PROPERTY`, `IOCTL_STORAGE_GET_DEVICE_NUMBER`, and `IOCTL_DISK_GET_LENGTH_INFO`. Confirmation is the full `\\.\PhysicalDriveN` path with model and capacity shown alongside it. Do not guess devices by probing numbered paths or identify a target merely by `E:`. Microsoft documents physical disk access through [CreateFileW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew) and [IOCTL_STORAGE_QUERY_PROPERTY](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_storage_query_property).

The current W5 test model is one `nandunx-desktop.exe` with `requireAdministrator`, requesting UAC at startup. The user chose this after earlier helper trials. Disk listing, preflight, and writer run in that process through core. It calls no `sudo`, exposes no HTTP service, and provides no general raw-I/O command. Elevating the full GUI also elevates WebView and file pickers, which needs acceptance testing. Full preflight, source-target comparison, volume locks, target reidentification, and exact path confirmation remain mandatory. Logs include operational details but no key values.

## Write barrier and recovery

Before adding Windows writing, [TRANSAKCJA_ZAPISU.md](TRANSAKCJA_ZAPISU.md) was expanded with the Win32 handle lifecycle, `CreateFileW` flags, locks, flush, and recovery. W1 tested exclusive RAW and a volume lock on disposable VHDX; actual reader behavior still needs hardware validation. No stage may skip these checks:

1. Enumerate read-only. Reject system/boot/pagefile disks, unsupported geometry, and ambiguous devices. Missing permissions or unreadable dependencies mean refusal.
2. Map every GUID volume, including letterless ones, to physical extents. Reject targets containing any backup part, keyset, boot file, working file, or executable. Keep all sources open and verified through the operation. In-place read access to the target is intentional, but its independent backup must remain elsewhere.
3. After full preflight, require the exact `\\.\PhysicalDriveN` text. Reidentify the disk, acquire and retain volume protection, check writable-handle identity, and verify no new volumes appeared. Protection must bridge read-only preflight and writable opening.
4. For visible volumes, `FSCTL_LOCK_VOLUME` must succeed before `FSCTL_DISMOUNT_VOLUME`; retain handles. Dismount or `fs2` alone cannot prevent remounting. Never force dismount of a volume that could not be locked. For RAW layouts with no visible volumes, require an exclusive unshared physical handle, a second-handle probe yielding `ERROR_SHARING_VIOLATION`, and repeat volume enumeration. Any failure or new volume refuses writing. Do not disable global automount or use `diskpart` as a bypass.
5. Preserve core phases: copy/verify, relocation, FAT, GPT, boot sectors. Write metadata after data, sync and reread at required boundaries. Verify `File::sync_all`/`FlushFileBuffers` on a physical handle, cache flags, and buffer/offset/length alignment. 512e needs its own test; a 16 KiB offset alone does not align buffer memory.
6. Stop on phase errors. Observe cancellation at pre-commit core boundaries. After final commit begins, do not claim rollback. On error or device loss, report last phase and require read-only manual inspection.
7. Release protection after verification or stopping. Do not automatically resume writes after process interruption. Retain the original backup. A geometry report is not a data backup.

Win32 API references: [volume extents](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-ioctl_volume_get_volume_disk_extents), [volume lock](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_lock_volume), [dismount](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_dismount_volume), [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers), and [I/O buffering/alignment](https://learn.microsoft.com/en-us/windows/win32/fileio/file-buffering). If interruption tests show unreadable old data before commit, the write gate stays closed until transaction or recovery design is fixed.

## Automated builds on GitHub and Gitea

Build natively on Windows x64. A shared `scripts/release/build-windows.ps1` is planned to accept a tag, validate Cargo/npm/Tauri versions, run `npm ci`, `npm run build`, `cargo fmt --check`, `cargo test --workspace --locked`, and Tauri NSIS packaging, then normalize artifact names and compute SHA-256. It must check native process exit codes but publish nothing itself. Keep Linux `build-artifacts.sh` and share version validation instead of duplicating rules.

Planned assets are `nandunx-<version>-windows-x86_64-setup.exe` and one `SHA256SUMS` spanning Windows and the three existing Linux archives. Windows gets no Web/headless package; MSI remains outside the first release. A Tauri Windows platform override should keep Linux `deb`. Installer tests must include a machine lacking WebView2 runtime. See [Tauri Windows installer](https://v2.tauri.app/distribute/windows-installer/) and [build prerequisites](https://v2.tauri.app/start/prerequisites/).

| Element | GitHub Actions | Gitea Actions |
| --- | --- | --- |
| Windows runner | Dedicated self-hosted `[self-hosted, Windows, X64, nandunx-release-windows]` | Native Windows runner labeled `nandunx-release-windows:host`; job `runs-on: nandunx-release-windows` |
| Shell | Explicit `pwsh` | Explicit `pwsh` after PowerShell 7 install and runner-version test |
| Build | Shared `build-windows.ps1` | Shared `build-windows.ps1` |
| Unpublished tests | Trusted changes and manual builds | Trusted changes and manual builds |
| Release | Tag, Linux/Windows builds, one artifact publisher with `contents: write` | Tag, Linux/Windows builds, one publisher with release permission |

Gitea has Windows runners, but default shell and external Actions compatibility must be tested on the particular instance: [Windows FAQ](https://docs.gitea.com/1.25/usage/actions/faq/) and [host mode/labels](https://docs.gitea.com/1.25/usage/actions/act-runner/). GitHub documents its own [runner registration](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners). These are separate processes and registrations, not one shared token. Prefer two reproducible VMs from the same image. If sharing a host, use separate accounts, directories, caches, and a global build lock; one service's `concurrency` setting cannot protect against the other.

Runner image: Git, Node 22, pinned Rust MSVC with rustfmt, Visual Studio C++ Build Tools and Windows SDK, PowerShell 7, WebView2 for smoke tests, and bundler tools. W1 found Rust 1.85 insufficient for the locked dependencies; the shared pin is 1.90.0. Record image/tool versions in each report. A provisional VM is 4 vCPU, 8–16 GiB RAM, and at least 60 GiB free for toolchains/cache, to be verified by actual builds.

Build runners use ordinary accounts and have no eMMC, BIS secrets, or real backups. Raw I/O tests are separate manually triggered jobs on a disposable VHD/test device with their own confirmation. Do not run untrusted PRs on a persistent runner with secrets. A code-signing certificate is a separate release secret: sign the EXE before bundling, then the installer, and compute checksums after signing. Beta builds may be explicitly unsigned; public releases need a signing/certificate/timestamp decision. Signing does not guarantee a SmartScreen-free launch.

Do not assume GitHub `upload-artifact`/`download-artifact` versions work on Gitea. W1 verified checkout, PowerShell, and artifact transport on the real Gitea instance. W6 will test GitHub after the functional Windows build, then select compatible versions or API transport. The publisher waits for both builds, verifies commit/tag, versions, and expected files, and emits one complete manifest. This avoids two jobs overwriting `SHA256SUMS`. A release requires all jobs to succeed; retries use the same tag. Equivalent versions and tests matter across services, though independent signatures/timestamps prevent a promise of byte-identical builds.

## Effort and acceptance

These are workdays for one Rust/Win32 developer with Windows hardware and disposable eMMC available, excluding waits for hardware or certificates.

| Stage | Estimate | Acceptance result |
| --- | --- | --- |
| W1 Windows/runner spike | 2–3 days | Native build/tests, VHDX geometry/locks, Gitea artifact |
| W2 platform boundary | 3–5 days | Linux unchanged; Windows has no permissive safety stubs |
| W3 device preflight | 4–6 days | Real-reader list/geometry/identity, sources and volumes, negative tests |
| W4 authorization and I/O | 5–8 days | RAW protection on target adapter, flush/recovery, fixture/VHD tests before eMMC |
| W5 full desktop | 4–6 days | One UAC EXE, three operations, progress, cancellation, report, key-free logs |
| W6 packages/release | 2–4 days | GitHub trial after functional Windows build, installer, automatic complete releases |
| W7 hardware acceptance | 4–7 days | Repeatable Windows/reader/eMMC report, Switch test, installation and recovery |
| Total | 24–39 days | First full Windows desktop release |

A 20–30% buffer gives roughly 6–10 weeks of one person's work. W3/W4 results may change estimates. Unstable reader identity, inability to protect RAW, or faulty flush can narrow supported adapters or delay release. The estimate is only for Windows GUI.

Minimum acceptance includes the common Linux/Windows suite; synthetic single/split/FULL NAND backups; both GPT/FAT copies and `USER` file contents after expansion. System tests cover no admin/UAC denial, busy volumes, source on target, system/pagefile targets, letterless volumes, device replacement under the same number and capacity, missing IDs, 512e and rejection of 4Kn, partial writes, flush errors, restart, and cancellation around commit. Tests for the former helper's IPC loss are historical; current one-EXE design must test forced termination instead. Windows pickers must handle Unicode, spaces, long paths, and `.00`/`.01` parts.

Hardware tests start on disposable VHD with a synthetic image, then disposable eMMC with a retained independent backup. Test each mode separately, independently reread the result, check Switch boot, visible capacity, and preserved data. Record Windows version, reader model, geometry, and results, without keys or backups. Disconnect tests use only expendable media. Hardware-free CI cannot certify controller flush; one adapter's success does not certify all others.
