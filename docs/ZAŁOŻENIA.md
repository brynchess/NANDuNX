# NANDuNX project assumptions

## Problem and intended result

NANDuNX restores an owner's Nintendo Switch NAND backup to a larger device and uses the extra capacity by expanding the `USER` partition. Expansion must preserve `USER` data without formatting it or requiring users to copy files back manually.

The first supported release is local on Debian-based Linux, as a Tauri desktop application and a local Web UI. Both use the same Rust engine and React/Vite frontend.

NANDuNX code is available under GNU GPL version 3 or later; see [LICENSE](../LICENSE). NxNandManager inspired the project, but NANDuNX independently implements a focused restore-and-expand workflow.

## Initial scope and supported inputs

1. Read-only recognition of RAWNAND/FULL NAND images, split images, and block devices.
2. Local keyset input with BIS keys and deterministic compatibility checks. A keyset is needed for `USER` expansion, but not for a byte-for-byte encrypted backup restore.
3. Preflight and a clear source/target/capacity/GPT/`USER`/permission/risk plan.
4. Restore to a larger device and automatically expand `USER` to the largest correctly aligned size.
5. In-place `USER` expansion on an already populated, physically larger device after the same checks.
6. Progress, cancellation at safe boundaries, and post-operation verification reports.

The initial hardware scope is Switch eMMC and readers available to the user. Linux may expose a reader as `/dev/mmcblk*` or, through USB, `/dev/sd*`. A node name does not prove device type. The engine enforces general block-device safeguards, while hardware testing covers controlled readers rather than certifying all adapters.

Users choose backup and keyset through GUI/Web UI, not by copying them into the repository or passing them on a command line. Desktop holds uncopied in-session file references. Web UI streams uploads to a private temporary process directory outside the repository; normal exit removes them and a new service session removes remnants from an interrupted prior one. Neither input is persisted in configuration.

Out of the initial Linux scope: obtaining keys, unrelated console-data modifications, public HTTP access, and unusual formats without fixtures and tests. Windows is a subsequent port with an unsigned preview installer; it is not yet a broadly validated release.

## Windows direction and current limits

The proposed first Windows release targets Windows 11 x64 with all three Tauri desktop operations, one EXE elevated by system UAC, and an NSIS installer. GPT/FAT/AES-XTS and the frontend remain shared. Win32 needs its own device-safety backend; Linux mountinfo and `fs2` locking are not substitutes. Windows is GUI only: no Windows Web UI, headless package, or service. Tauri renders the shared frontend locally without a NANDuNX HTTP server.

The initial Windows device profile requires 512 B logical sectors. It excludes 4Kn, BOOT0/BOOT1 writes, Windows 10, and ARM64. Native GitHub Windows runners must build without real backups, keys, or access to user devices. See [WINDOWS.md](WINDOWS.md) for the design and [ROADMAP.md](../ROADMAP.md) for the sole current stage status.

W1 validated MSVC compilation/tests, VHDX lock probes, and a downloadable Gitea artifact. W2 separated Linux and Windows device boundaries. W3 added read-only Win32 enumeration, 512/512e geometry, PnP/serial/GPT snapshots, and volume mapping. Preflight rejects missing durable IDs, unknown layout, 4Kn, LDM/Storage Spaces/RAID, system/pagefile disks, and source files on the target. A VHDX missing an ID was rejected as intended. W4 added protected RAW, source/keyset sessions, and an UAC/IPC helper; the helper was later replaced with a single administrator Tauri process. W5 repeats preflight and reports progress, cancellation, and results in the current session. One user-confirmed Switch restore-and-expand test passed; broader physical-reader validation remains open.

The Windows desktop writes a process log beside its EXE in `logs/`. It may contain device paths and operation phases, but no keys or keyset contents. Logs are not configuration and must not be committed. GitHub W6 uses hosted `windows-2022` and Ubuntu runners to assemble a tagged release after both native builds pass. Current `Cargo.lock` needs Rust 1.90 or newer; Linux and MSVC trials pin 1.90.0.

## GitHub release automation

Pushing an annotated `vX.Y.Z` tag to GitHub starts independent Linux and
Windows builds. The publisher job needs only `contents: write` on the
ephemeral repository `GITHUB_TOKEN`; no personal token or repository secret
is stored for release creation. It attaches the `.deb`, Windows NSIS `.exe`,
Linux Web/Docker archives, and a combined checksum manifest. GitHub provides
the source ZIP and tarball for the tag. The NSIS installer downloads WebView2
when required. Signing and fresh-install coverage are separate follow-up work.

## Implemented preflight and session UI

Preflight reads and CRC-checks primary GPT and `USER` from a single or numbered split RAWNAND. FULL NAND is recognized only as explicit `BOOT0 || BOOT1 || RAWNAND`, with 4 MiB BOOT0 and BOOT1 and RAWNAND primary GPT at exactly 8 MiB. It never scans for GPT or infers the offset from a filename. Desktop discovers later parts after selecting `.00`; Web UI uploads selected parts sequentially, each as its own stream, and polls server-confirmed written bytes. Separate BOOT0 and BOOT1 inputs for RAWNAND must be a complete pair of ordinary 4 MiB files. FULL NAND rejects this pair because the boot areas are already embedded.

For expansion, preflight decrypts `USER` metadata, verifies BIS Key 2 against its FAT32 boot sector, compares FAT mirrors, and counts allocated, free, and bad clusters. Both adapters offer restore, restore+resize, and in-place resize after mount checks, locking, and exact target-path confirmation. Desktop runs preflight and writes off the GUI thread, provides session status/progress, cancels before commit, and blocks concurrent jobs and window closing during work. Windows uses the same three commands through elevated Win32 sessions, subject to interactive and hardware tests.

The shared frontend separates **Preparation**, **Plan and checks**, and **Execution**. A persistent task panel shows phase, progress, cancellation, and a short session event list built from adapter responses and current actions. Events live only in frontend memory; they are not the diagnostic process log and disappear on reload. A Web UI reload cannot resume a running job.

The adapter supplies app version and edition: Windows/Linux desktop, Web, or Docker (set by `NANDUNX_EDITION=docker`). The browser's OS is not used as the edition. Windows uses `tauri.windows.conf.json` for a transparent undecorated window with custom title controls. Backend DWM enables Mica only if its call succeeds; on failure, diagnostics retain the HRESULT when available and the frontend uses a solid dark background. `window.close()` keeps the Rust operation-close guard. Linux and Web use normal controls. Overflowed panels show scrollbars.

## Automatic `USER` size

`USER` is the final data partition in the logical GPT area. For a device with `capacity_sectors` 512 B sectors and `user_start_lba`:

```text
available = capacity_sectors - user_start_lba - 33
user_size = floor(available / 32) * 32
```

The 33 reserved sectors hold backup GPT table and header. Alignment to 32 sectors gives a 16 KiB encryption unit/cluster. FULL NAND input has an 8 MiB RAWNAND offset (two explicit 4 MiB boot areas) before these values are used. Target sizes and LBAs remain in RAWNAND address space because the main Linux eMMC node excludes separate boot nodes. Reject plans that do not enlarge `USER`, have invalid GPT, or place another partition after `USER`.

## Preserving data during expansion

As FAT32 grows with `USER`, the data area starts later. This affects restore+resize and in-place resize. AES-128-XTS ties ciphertext to a 16 KiB unit index relative to the start of `USER`; a moved cluster must be decrypted at its old index and re-encrypted at its new index. Copying raw ciphertext would corrupt it.

Preflight validates both FAT copies and both matching FAT32 boot sectors, walks complete cluster chains, and rejects cycles, cross-links, and out-of-range references. It computes the new FAT length by fixed point because a larger FAT changes the data-area start and addressable cluster count. The complete relocation plan identifies each allocated cluster's old and new data offset. Relocation proceeds in descending cluster order, so it does not overwrite unread source clusters. Each affected 16 KiB unit is read, decrypted, re-encrypted for its partition-relative index, written, and verified; a cluster may cross unit boundaries.

The durable order is copy and verify RAWNAND (for restore), relocate data, write/verify both FATs, backup GPT, primary GPT, backup boot sector, and **finally primary FAT32 boot sector** as the geometry commit. Synchronize and reread at each required boundary. Before that final sector, the old primary boot geometry remains active, although some backup metadata may describe the new range. A partial final-sector write or sync failure may require manual recovery from the verified backup boot sector. There is no automatic rollback after commit. Details are in [TRANSAKCJA_ZAPISU.md](TRANSAKCJA_ZAPISU.md).

Core has synthetic in-memory and durable ordinary-file fixtures. They never require real NAND or keys. Fixture tests cover phase boundaries, partial writes, `sync_all` failures, and restart before each sync; a separate read-only parser classifies GPT and boot sectors. A fixture cannot model a particular controller cache or arbitrary power loss during real I/O. Hardware use remains a controlled test with a retained backup.

## Encryption compatibility

The reference point is the upstream NxNandManager commit `b106040e68faa5854d0d9c82613976c6d8fee97d`; later local experimental resize commits `a56e065` and `369a628` are excluded. Its observed mapping is `bis_key_00` for PRODINFO/PRODINFOF, `bis_key_01` for SAFE, and `bis_key_02` for SYSTEM/USER. Each BIS key has separate 128-bit `crypt` and `tweak` halves.

The compatible profile is AES-128-XTS with 16 KiB partition-relative units. The tweak half encrypts the unit index, the crypt half transforms 16 B blocks, and GF(2^128) multiplication advances the tweak. NANDuNX implements this independently with RustCrypto `aes` and a synthetic golden vector checked against OpenSSL EVP AES-128-XTS. No console data or reference code is copied into tests.

## Operational safeguards and acceptance

- Backups and keysets stay local. NANDuNX has no telemetry or upload outside the user's local loopback Web server.
- The headless service keeps one detailed redacted `last-run.log` for the current run. It records upload-session creation, completed parts and confirmed byte counts, preflight, phases, and coarse restore progress. It omits names, paths, contents, key material, and device identifiers.
- Web binds to `127.0.0.1`. `scripts/install-headless.sh` installs a privileged local systemd service so block access does not need manual ACL setup after every reconnect. Remote users use SSH tunneling; browsers receive no `sudo`, password, or general shell. HTTP requests are limited to structured NANDuNX operations that revalidate targets.
- Permissions do not replace full preflight, mount checks, exact path confirmation, or target reidentification before writes. Restore itself is not a backup; the UI should require confirmation of an independently retained source.
- Some adapters expose only the main eMMC user area. This does not block RAWNAND/`USER` work, but the plan must say that BOOT0 and BOOT1 require separate Hekate restoration.
- Windows W4/W5 implementation trials use synthetic files and disposable VHDX. A physical eMMC reader needs separate checks of identity, exclusive RAW protection, and I/O durability. One VHDX result does not establish reader compatibility.

Initial acceptance criteria: after expansion, all files in a test image remain readable with matching data hashes; `USER` reaches the largest valid size and both GPT copies are valid; wrong keys, invalid FAT/GPT, insufficient target capacity, or lack of permission stop before writes; interruption at every pre-commit stage leaves the old active layout mountable.
