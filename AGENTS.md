# AGENTS.md

## Project goal

NANDuNX simplifies restoring an owner's Nintendo Switch NAND backup to a larger device and automatically expands the final `USER` partition while preserving existing data. Linux (Debian and derivatives) is the first supported system. The same React frontend runs in the Tauri desktop app and through a local HTTP server.

## Safety boundaries

- Work only with backups and devices the user is authorized to use. Do not add security-bypass or key-acquisition features.
- BIS keys and backup files are secrets: do not log them, add them to fixtures, store them in configuration, or commit them. `.gitignore` is a mandatory second line of defense.
- Writing to a block device requires a plan, explicit confirmation containing its full path, and validation that source and target are different devices. Open devices read-only by default.
- Never run `sudo` inside the application. The operating system must grant access to the specific device (for example through udev/polkit), and the process must be explained to users.
- Complete preflight before changing GPT, FAT, or `USER`; write metadata changes last. An error or cancellation before metadata commit must leave the previous layout readable.

## Architecture

- `crates/nandunx-core` — pure, testable Rust engine for formats, planning, I/O, GPT, FAT32, and encryption. No Tauri, HTTP, or UI.
- `src-tauri` — thin Tauri desktop adapter calling only public `nandunx-core` APIs.
- `apps/nandunx-web` — local HTTP adapter. It binds to loopback only by default; remote binding needs an explicit option and, later, a token.
- `src` — shared React/Vite frontend in JavaScript, not TypeScript. Do not put NAND modification logic or keys in it.

## Encryption and compatibility

- Support only the profile confirmed by compatibility vectors: AES-128-XTS with 16 KiB units and two BIS key halves. Do not replace it with ordinary AES or change the unit size.
- Keyset parsing and cryptography need their own unit and golden-vector tests. The implementation must be independent of NxNandManager; do not copy its code.
- Validate keys against safe, deterministic partition data or metadata before enabling writes.

## Working standards

- Before implementing a destructive operation, document its plan, limitations, and recovery strategy in `docs/`.
- New `nandunx-core` logic requires tests. Tests must not require real NAND or real keys.
- Check Rust formatting with `cargo fmt --check` and test with `cargo test --workspace`; build the frontend with `npm run build`. Add a linter only with an agreed configuration.
- Backend commands and HTTP endpoints return structured data and progress; the UI does not interpret raw I/O errors.
- Log every system or external API error with the operation phase and original error code (for example Win32 `GetLastError`, Unix `errno`, or HTTP status) when the API supplies one. Preserve the code while mapping it to an application message. When no code exists, state that explicitly in the log. The frontend may show a shorter message; logs must not contain keys or backup contents.
- Update `README.md` for user-visible changes and `docs/ZAŁOŻENIA.md` for changed assumptions or limitations.

## Building and testing on Windows

- A Windows Server 2022 test server is available at `192.168.0.14`, SSH account `nandu-build`, port 22. Connect from this environment with `ssh -i ~/.ssh/nandu_windows_2022 nandu-build@192.168.0.14`. The private key stays outside the repository; never copy or commit it. The account has no administrator privileges.
- Use a separate checkout or an export of tracked files from the exact revision being tested. Do not transfer the whole working directory, NAND backups, keysets, or other secrets. Record the revision and command results in the report.
- Refresh `PATH` in PowerShell because an SSH process can have stale environment variables: `$env:Path = "$env:USERPROFILE\.cargo\bin;" + [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')`.
- Before tests, check `git --version`, `node --version`, `npm.cmd --version`, `cargo --version`, and `rustc -vV`. The `host:` line from `rustc -vV` must say `x86_64-pc-windows-msvc`; Chocolatey's `rust` package provides the GNU variant and does not qualify. If needed, install the MSVC toolchain through `rustup` for `nandu-build`.
- In the project directory run, in order, `npm.cmd ci`, `npm.cmd run build`, `cargo fmt --check`, `cargo test --workspace --locked`, and `cargo build -p nandunx-desktop --locked`. Stop and report real compilation or test errors; do not bypass safety checks with platform stubs.
- The current Tauri configuration packages only `deb`. A Windows/NSIS installer needs separate configuration in W6; `cargo build` alone does not verify GUI behavior or device access. Use no real NAND or keys on this server. Windows Server 2022 is for build trials; target Windows 11 and hardware tests have separate criteria in `ROADMAP.md`.

## Roadmap

[ROADMAP.md](ROADMAP.md) is the sole source of truth for current status, stage order, and hardware-test criteria. Update it in the same change whenever a stage is completed, split, or its scope changes.
