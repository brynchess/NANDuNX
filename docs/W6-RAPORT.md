# W6 — packaging and GitHub release automation

## Status, 2026-10-07

GitHub release automation is configured for annotated `vX.Y.Z` tags. It uses
hosted Ubuntu 22.04 and Windows Server 2022 runners. The Linux and Windows
jobs build independently; a final Ubuntu job downloads both artifact sets,
recreates one `SHA256SUMS`, and creates or updates the GitHub Release using
only the job's `contents: write` `GITHUB_TOKEN`.

The Windows NSIS build was validated from a separate export of tracked files
at revision `382da5e9f34055ce6d42e6f87a6285bbeb76277e` on Windows Server
2022 as `nandu-build`. The export contained no backups, keysets, or unrelated
working files. No eMMC was attached.

| Command or check | Result |
| --- | --- |
| `git --version`, `node --version`, `npm.cmd --version`, `cargo --version`, `rustc -vV` | Passed; Rust 1.90.0 with host `x86_64-pc-windows-msvc` |
| `scripts/release/build-windows.ps1 -Tag v0.2.0` | Passed on the isolated export |
| `npm.cmd ci`, `npm.cmd run build`, `cargo fmt --check`, `cargo test --workspace --locked` | Passed; 64 core and 6 desktop tests |
| Tauri NSIS bundle | Passed; `NANDuNX_0.2.0_x64-setup.exe`, 2.16 MiB |
| Normalized Windows asset | `nandunx-0.2.0-windows-x86_64-setup.exe`; SHA-256 `02297d2a0ece1d43bcb29b11c93557bd9d501b5952127f36b44841e8861f57de` |
| Linux `scripts/release/build-artifacts.sh v0.2.0` | Passed from a separate tracked-file export on durable local storage; produced `.deb`, headless Web, Docker/TrueNAS archive, and `SHA256SUMS` |

The first attempt to package the Windows installer rejected an invalid NSIS
configuration key before any release asset was made. The corrected key is
`installMode`; the successful test above used the corrected revision.

## Limits still open

The installer is unsigned. This stage did not test a fresh install,
uninstallation, an absent-WebView2 machine, or a Gitea Windows release
workflow. Those items, along with broader Windows hardware acceptance, remain
outside this automated build result.
