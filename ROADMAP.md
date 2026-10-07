# NANDuNX roadmap

This document is the sole source of truth for project status, stage order, and hardware-test gates. Update it in the same change that completes, splits, or materially changes a stage.

## Completed capabilities

- The UI offers RAWNAND restore, restore plus `USER` expansion, and in-place `USER` expansion. Both Web/headless and Tauri desktop call the same guarded `nandunx-core` writers.
- Users select a backup, keyset, and optional BOOT0/BOOT1 pair in the UI. Desktop keeps file references only for the session; local Web uploads go to a private process directory.
- Device listing is read-only. Plain restore can start preflight without a keyset; expansion requires one.
- When an adapter does not expose BOOT0 and BOOT1 devices, RAWNAND/`USER` work can proceed after preflight; the UI explains that boot areas must be restored separately with Hekate.
- Core validates primary GPT, CRC, and `USER` on synthetic fixtures, including across split parts `.00`, `.01`, and so on. It recognizes only explicit RAWNAND or FULL NAND (`BOOT0 || BOOT1 || RAWNAND` with two 4 MiB boot areas), without guessing offsets.
- Two common BIS keyset formats are parsed. Independent AES-128-XTS with 16 KiB units has a synthetic golden vector. Expansion preflight verifies BIS Key 2 against encrypted FAT32 boot metadata, compares FAT mirrors, and calculates expanded geometry by fixed point.
- The FAT32 parser walks full allocation chains, rejects cycles and cross-links, and produces a descending relocation plan. Expanded FAT and boot-sector preparation happens in memory.
- Synthetic restore+resize relocates allocated clusters with XTS re-encryption and updates both FATs, boot sectors, and GPT copies. Fault injection before each phase preserves the old readable layout. A fixture simulates restart before each `sync_all`, discards uncommitted phase data, and produces a read-only GPT/boot-sector recovery report without automatic repair.
- A headless systemd installer deploys loopback-only Web UI. The service keeps one redacted current-run log, omitting secrets, upload names and paths, and device identifiers.
- Docker Compose includes a multi-stage build, `.env` template, one-device passthrough, and read-only host `mountinfo` so container preflight sees host mounts. Safe Docker and TrueNAS SCALE YAML deployment is documented; a guided form without scoped device passthrough is not a write path.
- `vX.Y.Z` tags run GitHub and Gitea Linux release workflows producing `.deb`, headless Web, and Docker/TrueNAS archives with `README-TRUENAS.md`. The dedicated Gitea runner's requirements are documented. It has Docker Engine access and must run only trusted workflows.
- Web UI uploads backup parts sequentially as streams and polls confirmed server-written byte counts every 0.5 seconds.
- A controlled plain-restore hardware test passed on one eMMC reader: Web UI uploaded 15 parts, the writer copied and reread all RAWNAND, then verified final primary metadata commit. This does not certify other adapters.
- Core restore and restore+resize writers run asynchronously through both adapters, repeating preflight, requiring the full target path, reporting progress, and allowing cancellation before final primary boot-sector commit. Expansion relocates `USER` with AES-128-XTS/BIS Key 2. BOOT0/BOOT1 writes remain outside the writer.
- Desktop repeats full preflight and authorization before writing, sends progress/status/cancellation over IPC, and blocks concurrent operations or window closure during preparation and writing. Plain restore without a keyset correctly omits FAT32 metadata from its report.

## Desktop validation and Windows support

- [x] Connect all three Tauri modes to the same authorization and engine as Web.
- [x] Test session lifecycle, progress, cancellation, and concurrent-operation blocking.
- [ ] Perform controlled Tauri hardware tests for each mode: fully reread RAWNAND after restore, verify GPT/FAT and `USER` contents after expansion, and test pre-commit cancellation. Web hardware results do not validate the desktop adapter.
- [ ] Complete and validate Windows device identification, geometry, volume protection/locking, source-target isolation, and revalidation before writes with synthetic and physical tests before announcing support.

## Completed transaction and fixture stage

1. [x] Document transaction order and recovery. Add target reidentification, source-target isolation, mount checks, exact path confirmation, and exclusive read-only lock.
2. [x] Implement fixture-only transactional plain restore with progress, safe-boundary cancellation, and byte-for-byte verification; test RAWNAND, FULL NAND, and block-boundary cancellation. Device access required further fault-injection tests.
3. [x] Add fixture restore+resize and relocation/FAT/GPT/boot-sector checkpoints. Reread GPT and encrypted `USER` metadata after full commit. In-memory preparation alone was not a durable device transaction.
4. [x] Perform durable fixture phases without allocating the entire output: copy/verify, relocation, FAT, backup/primary GPT, backup/primary boot sector, `sync_all`, and reread after each phase. Primary boot geometry switches only at the final commit; fixture APIs still reject block devices.
5. [x] Inject partial writes and `sync_all` errors in every fixture phase. Distinguish pre-commit behavior from uncertain results after a primary boot-sector write, which require manual verification or recovery from the backup sector.
6. [x] Simulate restart before every `sync_all` and loss of current phase's unsynced data. Provide an independent read-only GPT/boot-sector recovery report, with possible manual recovery advice but no automatic repair.

## Current direction: Windows

On 2026-10-01 the user reported a successful test on a real Nintendo Switch. On 2026-10-07, the user confirmed that the Windows restore-and-expand flow completed successfully: the console recognized the expanded partition and existing files were preserved. These results confirm the tested hardware combination and close W5, but do not validate every reader, mode, or recovery path; W7 retains the broader release gate. The controlled plain-restore result above remains the detailed record.

[docs/WINDOWS.md](docs/WINDOWS.md) covers feasibility, scope, permissions, recovery, and sources. The proposed first Windows target is Windows 11 x64 with full Tauri desktop and an NSIS installer. Windows releases will be GUI only; Web/headless remain Linux options. A Windows HTTP server, Web package, and service are not planned. Windows is not yet generally supported.

The project follows one stage per session. Resolve doubts at the start, complete the stage before ending that work session, do not append extra stages, and summarize the finished stage.

| Stage | Status | Scope and completion gate |
| --- | --- | --- |
| W0 — port audit and design | Complete | Identified Linux dependencies, Win32/UAC requirements, GitHub/Gitea, scope, and estimates. Linux desktop writer works; Windows needed its own device backend and permission model. |
| W1 — Windows and CI spike (2–3 days) | Complete | MSVC workspace tests and desktop/frontend builds; blocker list; disposable VHDX geometry, identity, RAW/volume lock prototype; Gitea PowerShell checkout and downloadable artifact. Rust pinned to 1.90.0. Real reader moved to W3 and GitHub trial to W6. [Report](docs/W1-RAPORT.md). No eMMC writing. |
| W2 — platform boundary (3–5 days) | Complete | Split `device/linux` and `device/windows`, removed permissive `cfg(not(unix))`. Windows rejected enumeration, identification, volume mapping, authorization, and handle comparison until later stages. Linux and MSVC fixture/negative tests passed. [Report](docs/W2-RAPORT.md). |
| W3 — enumeration and preflight (4–6 days) | Complete without physical reader | SetupAPI/Win32 listing, geometry, PnP/serial/GPT snapshot, volumes/extents, source mapping including split parts. Rejects system/pagefile, 4Kn, LDM, RAID, Storage Spaces, and missing IDs. Synthetic tests and system-disk/VHDX read probes passed; VHDX without full identity was rejected. [Report](docs/W3-RAPORT.md). |
| W4 — authorization and durable I/O (5–8 days) | Programmatically complete without physical reader | Core sessions held volume locks, exclusive RAW handle, open sources/keyset, and ran three modes with identity/plan checks, `sync_all`, progress, cancellation, and read-only recovery. The earlier UAC helper used an ACL-limited local pipe and mutual PID checks. Synthetic fault/restart tests and three disposable VHDX scenarios passed. GUI UAC moved to W5; eMMC/adapter and disconnect durability to W7. [Report](docs/W4-RAPORT.md). |
| W5 — Windows desktop execution (4–6 days) | Complete | Desktop requests administrator at startup through `requireAdministrator`. Disk listing, preflight, and three write modes call guarded core sessions directly. Progress, cancellation, result, close blocking, and one redacted log remain; helper/IPC were removed. MSVC tests, embedded frontend, and EXE manifest passed. A full test archive exists. A full disk on the build server caused an earlier PDB error; the workflow now checks free space. On eMMC, the volume-free target initially blocked writes; exclusive RAW protection with a second-handle and volume-map recheck was added. Physical reads are sector aligned, and full restore+expand planning occurs before any write. The user completed the interactive Windows restore-and-expand acceptance on real hardware: Nintendo Switch recognized the expanded partition and existing files were preserved. [Report](docs/W5-RAPORT.md). |
| W5.5 — shared interface (all editions) | Programmatically complete; visual/interactive acceptance pending | Replaced one scrolling page with Preparation → Plan and checks → Execution. A fixed panel shows task, phase, progress, cancellation, and session events; header/sidebar show edition and version. Events stay in browser memory and contain no keys or backup contents. Windows has custom title controls and Mica with dark fallback; other editions keep native controls. Scrollbars appear as needed. Linux frontend build and workspace tests passed. Windows interactive acceptance belongs to W5. The Gitea Windows runner was restarted and moved to a separate `D:` work volume. [Runner report](docs/W5.5-RUNNER.md). |
| W6 — packaging and automatic release (2–4 days) | In progress | GitHub uses hosted Ubuntu and `windows-2022` runners, shared Windows PowerShell packaging, Tauri NSIS/WebView2 configuration, and one publisher that combines assets and `SHA256SUMS` only after both builds pass. The initial tag verifies checkout, artifact transfer, and release creation. Fresh installation/retry, code signing, and a Gitea Windows release workflow remain open. Build runners have no NAND, keysets, or eMMC. |
| W7 — hardware tests and release (4–7 days) | Planned | Test restore, restore+resize, and in-place on disposable eMMC with an independent backup. Independently check GPT/FAT, data hashes, console boot, and available capacity. Test controlled cancellation/recovery on expendable media. Record Windows/reader/geometry, installation/uninstallation, and releases from both services before declaring Windows support. |

Order: W1 → W2 → W3 → W4 → W5 → W5.5 → W6 → W7. W6 infrastructure may be prepared earlier, but an automated build does not unlock writing. Failure of device protection, I/O durability, or preservation of data before commit blocks writer deployment until corrected or support is narrowed. A user test report is not expected to cover every failure mode.

## After the first Windows release

- Windows 10 and ARM64 require separate toolchain, dependency, and hardware validation; MSI, auto-update, and broader reader coverage are separate work.
- BOOT0/BOOT1 writing remains outside the Windows port.
