# W5 — Windows desktop execution

Status on 2026-10-05: implementation and automated tests passed. Interactive GUI acceptance and full hardware validation remain open, so W5 is not closed.

## Hardware observations and fixes

The user tested an eMMC reader on Windows. In the one-EXE build, disk listing and preflight worked, but write authorization returned `CannotProtect` before the writer started. The earlier log did not distinguish missing target volumes, handle opening, locking, dismounting, or opening RAW for writing. The log now records the precise step, volume number, and Win32 code; the UI still shows a short message.

A second attempt identified `no target volumes found` as the `CannotProtect` step. Read-only preflight passed; the writer did not start. A separate protection path for this layout now requires an exclusive physical handle, identity verification, a second-handle probe that must return `ERROR_SHARING_VIOLATION`, and another volume enumeration. Ambiguous results block writing. A disposable VHDX with GPT and an MSR partition but no volume passed the exclusive RAW-handle probe without writing sectors. An empty VHDX with no partition entry was rejected by the existing layout parser. The VHDX result did not replace a physical-reader test.

The subsequent eMMC test (log `target/5176e14-windows.log`) passed authorization, copied and verified the full RAWNAND payload byte for byte (`31,268,518,912` bytes), then verified primary metadata (`17,408` bytes). Only afterward, `WindowsRestoreSession::execute` failed to build the `USER` expansion plan and returned `Planning`. Relocation, FAT, and new GPT phases never began. Read-only in-place resize preflight on the same target also returned `Planning`. Full resize planning must happen before copying. The plan is now built from the locked source before opening the target for writing, then reused after full copy verification.

A diagnostic build narrowed the planning failure to `PlanningDetail(WriteFailed)` during read-only preflight. The old `read_fixture_at` mapped a failed `seek` or `read_exact` to `WriteFailed` without offset or OS code. Diagnostics now log the read offset, length, stage, and Win32 error without logging bytes. Read-only preflight on the physical reader revealed `ReadFailed { offset: 1024, length: 1408, step: "read", os_error: Some(87) }`. The GPT table has eleven 128-byte entries, but direct physical-disk reads on Windows require full sectors. Planning, verification, and recovery now read enclosing whole sectors (1536 bytes in this case) and pass only the requested 1408 bytes to the parser. GPT writes were already sector aligned. Synthetic tests also cover an unaligned starting offset. A physical read probe was added to the protected-handle VHDX test, but the non-admin build account cannot run that device test.

The next target-reader check is a read-only in-place preflight with the sector-read fix. MSVC compilation alone does not validate this adapter's behavior.

## Current single-EXE architecture

By user decision, the Windows desktop is one EXE with a `requireAdministrator` manifest; UAC appears at application startup. The prior `nandunx-helper.exe`, IPC pipe, and separate helper log were removed. Disk listing, preflight, and all three write modes call public `nandunx-core` sessions directly. Full source/target rechecks, volume locks, and exact device-path confirmation still precede writing. Beside the EXE, `logs/nandunx-desktop-<PID>.log` records authorization, phases, progress, and errors without key values or keyset contents.

An earlier linker failure, `LNK1318: PDB; LIMIT (12)`, occurred when the Windows server had 0 GiB free. Two temporary build directories occupied about 12 GiB. Removing them freed about 10 GiB; linking passed. The workflow now checks for at least 8 GiB free before building. Earlier results for the helper architecture do not validate the single-EXE architecture.

The single-EXE build was validated on Windows Server 2022 with `nandu-build`, Rust 1.90.0, host `x86_64-pc-windows-msvc`, from tracked files at `HEAD 17da5a51e867f1dd84a483193986d57a93b84a7d` plus local changes and a new logger. Source archive SHA-256: `727a0664ac0d756dcdcbd9621f80aef0321b880b571cbc7cb7ab70c749296021`. In order, `npm.cmd ci`, `npm.cmd run build`, `cargo fmt --check`, `cargo test --workspace --locked`, `cargo build -p nandunx-desktop --locked`, `cargo test -p nandunx-desktop --features custom-protocol --locked`, and `cargo build -p nandunx-desktop --features custom-protocol --locked` passed. The workspace ran 58 core and 6 desktop tests; custom protocol ran 7 desktop tests, including embedded `index.html`. Conditional VHD/eMMC tests did not run without explicit test variables. SDK `mt.exe` confirmed `requireAdministrator` and Common Controls in the built EXE.

The single-EXE test archive `nandunx-0.1.1-w5-single-exe-windows-x86_64-test.zip` has SHA-256 `2cd1d9988f28f350a645be2cedf6e8426173c656f4cf7d2e99e6690ce4219096`. It contains `nandunx-desktop.exe`, `README-TEST.txt`, and `REVISION.txt`, without a helper. EXE SHA-256: `efe9f5562563eafbadfa4aeadc00511fa2f427458748a6d8dc5d6df1f69d1020`. Package and full command transcript were copied to ignored local `release/`. Test sources and secrets are not in the package.

## Diagnostic build record

| Purpose | Source/export SHA-256 | Checks and test artifact |
| --- | --- | --- |
| Volume-free RAW protection | `897dc7fe95c4daf1ab8318df975fa6add9b2430f198648b2a48974cd3cf3b498` | Windows MSVC: required five commands, 60 core tests, custom-protocol build, and `scripts/windows/w5-empty-vhd-probe.ps1` passed. `release/nandunx-0.1.1-w5-no-volume-raw-windows-x86_64-test.zip` SHA-256 `4dec0cb0872af12f392f51decc0a38a884caa1e3571e6dc93e96c28a1fc4e4eb`; EXE SHA-256 `a030a648a7c3d7cbb05bb0264ae4056db747021ca402fb8a4988f3c6e29b596c`. |
| Read-offset diagnostics | `e920a565b66fd8294afc0abe01a6981fc70f4f0faa27c27fadcd300d686f6aef` | Required commands and custom-protocol build passed. `release/nandunx-0.1.1-w5-read-offset-diagnostic-windows-x86_64-test.zip` SHA-256 `2874e054e856a3ab0c1224cff1229256f0990347aeb16ed4c860bf3d7e2d42cf`. |
| Sector-aligned reads | `5879fd3d98066801a926233b17151a2531b3a0069eac536f7c35ccab24e13a37` | Linux formatting, workspace tests, and frontend build passed. Windows required commands and custom-protocol build passed. `release/nandunx-0.1.1-w5-sector-read-windows-x86_64-test.zip` SHA-256 `ef0901e8ea837d146a47d07c8602ecc6e9bae8dec2a749354a09e56a1534f063`. |
| Protection logging | `67fcff8226800641dfb1a4c903eda2ff1239fc66863406c58fa2cee28f6795dd` | Windows five commands and custom-protocol build passed; 59 core tests. `release/nandunx-0.1.1-w5-protection-diagnostics-windows-x86_64-test.zip` SHA-256 `04ada98b2c8e268226d286c0e0208026e1e6e0895dbd34696702a2edcfa7f760`. |

These builds used the same `HEAD 17da5a51e867f1dd84a483193986d57a93b84a7d` plus local changes. Logs are in ignored `release/` files. The diagnostic build itself did not write real eMMC.

## Earlier helper architecture and tests

The earlier desktop stored a selected Win32 disk snapshot. The helper launched through system `runas`, rechecked source mapping, target identity, and NAND/USER structure, then repeated authorization on a protected handle. Paths and requests passed through IPC, never command arguments or config. The worker returned status, progress, and cancellation; the GUI stayed responsive and blocked closing during preparation or writing. UAC refusal, IPC loss, or helper failure produced an error. Loss of the UI process stopped the helper at the next safe boundary; failures around commit required independent media verification. The picker selected `.00` and core discovered later parts. Tests covered two parts in a Unicode path longer than 260 characters, but not interactive picker clicking. Logs beside both EXEs contained stage/progress/errors, no keys, and could include a device path.

On revision `4779bfa`, Linux `cargo fmt --check`, workspace tests, and frontend build passed. Windows Server 2022 MSVC passed `npm.cmd ci`, `npm.cmd run build`, `cargo fmt --check`, `cargo test --workspace --locked`, and builds of desktop and helper; synthetic Unicode/split-backup and five named-pipe tests passed. Interactive picker, UAC grant/denial, window closing, and GUI write results remained pending. The tracked-file export SHA-256 was `7fd7ee176940fac9803689e6554aa13ad4cdedadb756035c89b7a40ac66f7a47`. Server tools: Git 2.56.0.windows.1, Node 24.21.0, npm 11.19.0, Rust/Cargo 1.90.0, MSVC host. There was no real NAND or keys. `cargo build` did not prove GUI function; installer work was W6.

A diagnostic export from that architecture at the same HEAD plus local changes had SHA-256 `3828f5deb7e359e5b6e8c47153763d161c679b13f996b936bad946474a10e5cc`. `npm.cmd ci`, `npm.cmd run build`, `cargo fmt --check`, and `cargo test --workspace --locked` passed, but desktop linking stopped at `LINK : fatal error LNK1318: Unexpected PDB file error; LIMIT (12)`. Later commands were not run and that final snapshot was not considered fully built. A previous snapshot (`c4d4fcd32826533cfa318ab02b23ecee97ed3c3c7cfed4f950be77f5baee21de`) built but lacked the final selected-target resolution and progress-log cap.

The first Gitea `v0.1.1` test package used ordinary `cargo build` and attempted to open `localhost:5173`; the user observed `ERR_CONNECTION_REFUSED`. Do not use it. A corrected `windows` branch build enabled `custom-protocol`, embedded the frontend, and verified `index.html`. Gitea [run #15](https://gitea.jawba.xyz/bartosz/upgrade_nx_nand/actions/runs/15) passed on revision `5176e14`, including workspace tests, `standalone_build_embeds_the_frontend`, and both EXEs. Archive `nandunx-0.1.1-5176e14-windows-x86_64-test.zip` has SHA-256 `c88a98ce34328c733655754796e4b49db36c7bfe7789bcb3bc5fb95c33c95460`. Windows 11 window launch still needed interactive acceptance.

## Remaining W5 acceptance

On controlled Windows 11 x64 with a disposable device and independent backup:

1. Build with `npm.cmd run build` and `cargo build -p nandunx-desktop --features custom-protocol --locked`. Launch `target\debug\nandunx-desktop.exe`, choose a Unicode-path split `.00` backup and keyset through the picker, and confirm part count in preflight. UAC should appear at EXE startup.
2. Refuse UAC at startup; the app should not open.
3. On a device that may be erased, check UAC grant, progress, cancellation before commit, window-close blocking, and the result. After an error, independently read GPT/FAT and assess recovery before another attempt.

Record results without backups, keysets, serial numbers, or complete private paths. Detailed eMMC, adapter, and durability tests belong to W7.
