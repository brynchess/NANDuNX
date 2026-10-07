# W2 — platform boundary

Status on 2026-10-03. The tested tracked-file export was revision `b17982f57f31c3640384f5900ec462717fc25d89`. `nandunx-core` separated `device/linux` and `device/windows` behind a shared platform gate. Reads of `/sys/block`, `/sys/dev/block`, and `/proc/self/mountinfo` exist only in the Linux module. Existing Linux behavior and mountinfo parser tests were retained.

The permissive `cfg(not(unix))` path for comparing locked and writable handles was removed. At W2, Windows explicitly refused enumeration, target identification, volume mapping, and handle comparison. Preflight and authorization returned `UnsupportedPlatform`; the plan marked writes unavailable. The rejection was enforced in core even if an adapter called its API without checking the GUI status. Other unsupported systems retained the same closed gate.

W2 did not add physical Windows disk writes. W3 would add safe enumeration, geometry, and read-only preflight. W4 covered authorization, volume protection, and durable writes. Windows remained unsupported until these checks existed.

## Validation

| Environment | Result |
| --- | --- |
| Linux `cargo fmt --check` | Passed |
| Linux `cargo test --workspace --locked` | Passed: 52 core and 6 desktop tests |
| Linux `npm run build` | Passed |
| Windows MSVC, five commands from AGENTS.md | Passed: `npm.cmd ci`, `npm.cmd run build`, `cargo fmt --check`, `cargo test --workspace --locked` (48 core, 6 desktop), `cargo build -p nandunx-desktop --locked` |

The Windows Server 2022 environment had Git 2.56.0.windows.1, Node 24.21.0, npm 11.19.0, Rust/Cargo 1.90.0, and host `x86_64-pc-windows-msvc`. Only tracked files from the stated revision were exported. No working directory, backups, keysets, or secrets were transferred; no eMMC or GUI was used.

Windows tests checked rejection of enumeration, preflight, restore authorization, and in-place resize authorization against a synthetic disk description. Shared fixture tests ran natively without real eMMC or keys.
