# W1 — completed Windows MSVC trial

Status on 2026-10-03. Builds ran on Windows Server 2022 (`10.0.20348`) as `nandu-build` from a separate export of tracked files at revision `bdd75f04c66ab27e7716e0fb77c2a92128636299`. The export contained no backups, keysets, or unrelated working files. No eMMC was attached. A temporary Gitea runner was subsequently registered on the server for W1 tests.

## Build results

| Check | Result |
| --- | --- |
| Tools | Git 2.56.0.windows.1, Node 24.21.0, npm 11.19.0, MSVC Rust 1.99.0; host `x86_64-pc-windows-msvc` |
| `npm.cmd ci`, `npm.cmd run build` | Passed |
| `cargo fmt --check` | Passed |
| `cargo test --workspace --locked` | Passed: 52 core tests and 6 desktop tests |
| `cargo build -p nandunx-desktop --locked` | Passed; GUI was not launched |
| Rust 1.85.0 with locked workspace | Blocked by dependency MSRV; Tauri 2.12.0 needs 1.90 |
| Rust 1.90.0 with W1 script | All five commands passed locally and in Gitea CI |

Node 24 was installed on the server. The experimental GitHub workflow pins Node 22, but its run was deferred to W6. The minimal rustup profile does not contain `rustfmt`, so trial workflows install it explicitly. The current `Cargo.lock` needs at least Rust 1.90; Linux and Windows builds pin 1.90.0.

## Observed blockers and limits

- `Get-Disk` initially returned `Access denied` because the account was not an administrator. After privileges were granted, a new SSH session had an active administrator token. `Get-Disk` saw only the 80 GiB QEMU SATA system disk with 512 B logical and physical sectors; no eMMC reader was connected.
- The disposable [VHDX probe](W1-VHD-PROBE.md) passed on a 64 MiB, 512 B logical / 4096 B physical, `File Backed Virtual` disk. Exclusive read-only RAW `CreateFileW` succeeded on the empty VHDX; a second handle failed with Win32 `32` (`ERROR_SHARING_VIOLATION`). After creating NTFS, `FSCTL_LOCK_VOLUME` succeeded; a second volume handle failed with Win32 `21` (`ERROR_NOT_READY`). The VHDX was detached. This does not prove protection for a real RAW reader without volumes; W4 retains that gate.
- Gitea Runner 2.3.0 was registered as `nandunx-w1-win2022` with label `nandunx-release-windows:host`. The historical scheduled task `NANDu-W1-Gitea-Runner` started it at Windows boot as `nandu-build`. The trial job uses Windows PowerShell.
- Gitea [run #9](https://gitea.jawba.xyz/bartosz/upgrade_nx_nand/actions/runs/9) checked out code but failed because `rustup` was absent from the job's `PATH`. After correction, [run #10](https://gitea.jawba.xyz/bartosz/upgrade_nx_nand/actions/runs/10) passed checkout, Rust and `rustfmt` installation, all five build/test commands, and `actions/upload-artifact@v3`. The downloaded `windows-w1-report` artifact records revision `9a839c7ad08722ebdf66b32fe26298f8ea8098dd`, Rust 1.90.0, and `result=passed`. The GitHub job was deferred to W6.
- The Windows build did not validate device handling. At W1, `nandunx-core` still used `/sys/block`, `/proc/self/mountinfo`, and `/sys/dev/block`; `ensure_same_authorized_device` had a permissive `cfg(not(unix))` branch. W2 needed to remove it before any Windows write path.
- `cargo build` did not verify Tauri launch, disk access, NSIS, or `FlushFileBuffers` semantics. No Windows write operation was enabled.

## Deferred tests

On 2026-10-03 the user deferred the GitHub checkout/artifact/`upload-artifact@v4` job until a functional Windows build. Merely having a workflow file was not evidence that it worked.

The test server had only a system disk. Reading a real reader's geometry and durable identity moved to W3; protection of a physical RAW reader remained a separate W4 gate. No eMMC was written. The 2–3 day W1 estimate excluded waiting for permissions and runner setup. The provisional remaining W2–W7 estimate was 21–36 days; the VHDX result did not reduce W4's estimate before testing the target adapter.
