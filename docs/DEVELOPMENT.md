# Developing NANDuNX

The README explains how to use NANDuNX. This document contains source-build requirements and commands. Contributor rules are in [AGENTS.md](../AGENTS.md), the current status in [ROADMAP.md](../ROADMAP.md), and the write-transaction design in [TRANSAKCJA_ZAPISU.md](TRANSAKCJA_ZAPISU.md).

## Project structure

| Path | Purpose |
| --- | --- |
| `crates/nandunx-core` | UI-independent Rust engine |
| `src-tauri` | Tauri desktop adapter |
| `apps/nandunx-web` | Local HTTP adapter |
| `src` | Shared React/Vite frontend in JavaScript |
| `docs/ZAŁOŻENIA.md` | Scope, algorithms, and safety criteria |
| `docs/TRANSAKCJA_ZAPISU.md` | Write phases and recovery |
| `docs/RELEASES.md` | Release builds and runner requirements |

The desktop and Web UI icon comes from `src-tauri/icons/source.png`. Generate Tauri icon sizes with `npm exec tauri icon -- src-tauri/icons/source.png -o src-tauri/icons`; the Web UI uses `public/nandunx-icon.png`.

## Requirements and local builds

Debian or Ubuntu development needs Rust 1.90+, Node.js 22+, and the Tauri/WebKitGTK libraries. After installing the toolchains:

```bash
npm ci
npm run build
cargo fmt --check
cargo test --workspace
```

Run the desktop app during development:

```bash
npm run tauri dev
```

Build a debug Debian package:

```bash
npm run tauri build -- --debug
```

The result is in `target/debug/bundle/deb/`. Only `.deb` packaging is configured at present.

Run the local Web adapter after building the frontend:

```bash
NANDUNX_WEB_ROOT=dist cargo run -p nandunx-web
```

It listens at `http://127.0.0.1:4321`; remote binding is rejected.

## Headless installation from source

The installer deploys the local server as a systemd service. An administrator runs the installer; the script itself does not invoke `sudo`. The installed service runs as root to access the selected block device.

```bash
npm ci
npm run build:web
cargo build --release -p nandunx-web
sudo ./scripts/install-headless.sh \
  --binary "$PWD/target/release/nandunx-web" \
  --web-root "$PWD/dist"
```

From another machine, use an SSH tunnel:

```bash
ssh -L 4321:127.0.0.1:4321 user@headless-host
```

Then open `http://127.0.0.1:4321` locally. Uploads are stored in a private session directory under `/var/lib/nandunx-web/uploads`; the current redacted log is `/var/lib/nandunx-web/last-run.log`.

## Windows and releases

Windows is in the test phase. A successful build alone does not establish that the GUI or device access works. Build a test EXE with the embedded frontend:

```powershell
npm.cmd run build
cargo build -p nandunx-desktop --features custom-protocol --locked
```

The Windows desktop requests administrator access through UAC. Test EXE logs go to `logs/nandunx-desktop-<PID>.log` beside the program. Test history, requirements, and support criteria are in [WINDOWS.md](WINDOWS.md) and [ROADMAP.md](../ROADMAP.md).

A `vX.Y.Z` tag currently builds Linux assets: `.deb`, Web/headless and Docker/TrueNAS archives, plus checksums. The process and runners are documented in [RELEASES.md](RELEASES.md). The internal Windows test artifact is not a user release.

## Tests and safe changes

Test new `nandunx-core` logic with synthetic data. Never use real NAND or keys in tests, fixtures, or the repository. Read [TRANSAKCJA_ZAPISU.md](TRANSAKCJA_ZAPISU.md) before changing the write path. [ROADMAP.md](../ROADMAP.md) records results and remaining hardware-test gates.
