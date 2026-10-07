# W4 — Windows authorization and durable I/O

Status on 2026-10-04. W4 started from `66eced7` (W3) and completed programmatic work on synthetic images and disposable VHDX devices. The user will test real eMMC on a disposable NAND when the application is ready. The reader was not passed through to the Windows test server, so the result is not hardware compatibility evidence. Full Windows validation used revision `c28d23bc889eba28cecd884a7faa81fcdecd0562` exported through `git archive`; archive SHA-256: `3fc030579f940baeccfc33519835a5e7ae9d7f3369f219bff853ec9d7c971c3e`.

## Implementation at W4

- `WindowsProtectedDisk` repeated W3 preflight across all sources, required literal `\\.\PhysicalDriveN` confirmation, locked and dismounted all volumes, opened RAW exclusively with write-through, checked disk number, geometry, GPT ID, PnP/serial, and retained locks throughout the session. Disks without volumes were rejected at this stage. `sync_all` used the same handle.
- Public Windows sessions held source files and keyset open with read-only sharing. Restore+resize checked GPT, key, both boot-sector/FAT copies, complete chains, and relocation plan before writing. In-place resize planned read-only and compared the plan again on the protected handle. All three modes used common writer phases.
- At W4, `nandunx-helper.exe` was a separate process elevated through system `runas`. A random local named pipe had explicit ACLs for the user SID, Administrators, and SYSTEM, rejected remote clients, and checked each peer PID. JSON frames were limited to 64 KiB. CLI arguments held only the pipe identifier and PID; backup, keyset, and confirmation passed through the pipe. The helper repeated authorization, returned structured progress/results, and detected cancellation or lost IPC. W5 was to connect it to Tauri.
- `inspect_windows_recovery` read GPT, boot sectors, and FAT from a reidentified disk after an error or restart without repairing anything. Errors during or after commit required manual verification.

## Validation

| Check | Result |
| --- | --- |
| Linux `cargo fmt --check`, `cargo test --workspace --locked`, `npm run build` | Passed: 53 core and 6 desktop tests |
| Windows MSVC `npm.cmd ci`, `npm.cmd run build`, `cargo fmt --check`, `cargo test --workspace --locked`, `cargo build -p nandunx-desktop --locked`, `cargo build -p nandunx-helper --locked` | All passed: 57 core, 6 desktop, 3 helper IPC tests |
| Helper IPC as a separate process | Mutual PID authentication and roundtrip, foreign PID rejection, structured failure without I/O |
| Disposable 64 MiB VHDX restore | Lock, synthetic RAWNAND write and flush, independent reread |
| Disposable 64 MiB VHDX in-place | Restore, resize, independent GPT and decrypted FAT32 read, `Committed` recovery report |
| Disposable 64 MiB VHDX cancellation | Stop after first payload block; old primary GPT unchanged |
| Synthetic writer fault injection | Partial writes, `sync_all` errors, restart before each phase sync, read-only recovery report |
| Identity change/hotplug | Snapshot rejected changed PnP/GPT ID and 4Kn; physical hotplug was not tested |

The server used Windows Server 2022, a non-admin account, Git 2.56.0.windows.1, Node 24.21.0, npm 11.19.0, Rust/Cargo 1.90.0, and host `x86_64-pc-windows-msvc`. The export included only tracked files. No real NAND or keys were present. The VHDX script detached and removed its own media in `finally`.

## Result limits

Named-pipe tests used an ordinary child process. The SSH account could not exercise interactive UAC; launch, denial, and IPC loss during GUI operation remained W5 criteria. VHDX confirmed Win32 I/O and engine behavior on a virtual disk, not physical controller cache flush, hotplug, or eMMC power loss. Hardware tests were deferred by agreement with the user. At W4, the Windows desktop still exposed no write operations until W5 connected the helper and sessions.
