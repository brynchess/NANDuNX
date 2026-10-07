# Release automation and runner requirements

GitHub release automation builds Linux and Windows assets natively, then uses
one publisher job to create the release only after both builds succeed. The
shared PowerShell build script validates versions, builds the NSIS installer,
and publishes nothing itself.
Windows assets contain the GUI installer only; Web/headless and Docker assets
remain Linux-only. Runner labels, prerequisites, isolation, signing and Gitea
compatibility checks are described
in [WINDOWS.md](WINDOWS.md); delivery status is in
[ROADMAP.md](../ROADMAP.md). Windows installer signing and broader hardware
acceptance remain release gates; the current installer is explicitly unsigned.

For the internal W5 GUI trial, Gitea runs `windows-w5-build.yml` on the
`windows` branch and tag `v0.1.1`. Earlier unsigned debug test archives
contained `nandunx-desktop.exe` and `nandunx-helper.exe`; the current W5 test
architecture uses a single elevated desktop EXE. These artifacts are separate
from versioned release assets and are not validated Windows installers. The
tag also starts the existing Linux release workflow on Gitea; it is not pushed
to GitHub by this repository's `origin` remote.

The first `v0.1.1` Windows test artifact was built without Tauri's
`custom-protocol` feature and incorrectly opened the development URL on
`localhost`. Do not use that tagged artifact. The corrected `windows` branch
workflow builds the frontend into the EXE and tests that `index.html` is
embedded. The immutable `v0.1.1` tag remains at its original commit; use the
newer branch artifact for the GUI trial.

Pushing an annotated tag matching `vX.Y.Z` to GitHub starts the release
workflow:

- Linux assets are built on a hosted Ubuntu 22.04 runner;
- the NSIS installer is built on a hosted `windows-2022` runner;
- a final Ubuntu publisher downloads both artifact sets, recreates one
  `SHA256SUMS`, and creates or updates the GitHub Release with the repository
  `GITHUB_TOKEN` limited to `contents: write`.

GitHub automatically attaches `Source code (zip)` and `Source code (tar.gz)`
for the tag. Pushing the tag to Gitea remains optional and currently runs its
separate Linux-only workflow.

The tag must exactly match the version in root `Cargo.toml`, `package.json`
and `src-tauri/tauri.conf.json`. The shared build script rejects a mismatch
before it creates an asset. A successful release contains:

| Asset | Contents |
| --- | --- |
| `nandunx_<version>_amd64.deb` | Desktop Tauri package for Debian/Ubuntu amd64 |
| `nandunx-<version>-windows-x86_64-setup.exe` | Unsigned NSIS installer for Windows x64 |
| `nandunx-web-<version>-linux-x86_64.tar.gz` | Headless `nandunx-web`, compiled frontend, systemd unit and installer |
| `nandunx-docker-<version>-linux-x86_64.tar.gz` | Saved Docker image, Compose, `.env.example`, Dockerfile and `README-TRUENAS.md` |
| `SHA256SUMS` | SHA-256 checksums for all binary assets |

Create a release only from a reviewed commit:

```bash
git tag -a v0.2.0 -m "NANDuNX v0.2.0"
git push github v0.2.0
```

Do not move or reuse a published version tag. A rerun replaces only assets with
the same names on the existing release; it does not change the tagged commit.

## GitHub Actions runner

No self-hosted runner setup, PAT, or repository secret is necessary. The
workflow uses GitHub-hosted `ubuntu-22.04` and `windows-2022` images; it
installs Node 22, Rust 1.90 and Tauri's Linux build dependencies. The publish
job requests only `contents: write` on its ephemeral `GITHUB_TOKEN` for
release creation and asset upload. Actions must be enabled for the repository;
if organization or repository policy blocks hosted runners or write tokens,
the failed run will report that policy.

For a self-hosted GitHub runner, install the same Linux prerequisites listed
below and ensure the runner account can execute `docker build` and `docker
save`. Do not grant that capability to workflows from untrusted forks.

## Gitea Actions runner

Use a dedicated x86_64 Debian 12 or Ubuntu 22.04 build machine, not the NAS,
and restrict it to repositories whose workflow code you trust. This workflow
builds Docker images; anyone who can modify a workflow executed by the runner
can effectively control its Docker daemon.

Requirements:

- Gitea 1.21+ with Actions enabled and a current Gitea Runner (2.x).
- A repository or organization runner registration token, plus a runner label
  `nandunx-release-linux:host`.
- Docker Engine running; the runner user must be able to run `docker version`
  without `sudo`.
- Git, curl, Python 3, Node.js 22+, Rust 1.90+, a C toolchain, pkg-config and
  the Tauri/WebKitGTK development packages.
- The Gitea job token must be allowed `releases: write`. In restrictive
  instances, increase the maximum Actions token permission for the repository
  or organization before triggering the tag.

### Install dependencies (Debian 12)

Run these commands as an administrator on the dedicated runner host. The
`gitea-ci` account owns the runner and its Rust toolchain.

```bash
apt-get update
apt-get install --yes --no-install-recommends \
  ca-certificates curl git python3 build-essential pkg-config libssl-dev \
  libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
  docker.io docker-compose-plugin
systemctl enable --now docker
useradd --create-home --shell /bin/bash gitea-ci
usermod -aG docker gitea-ci
```

Install the current Node.js 22 release globally and verify its checksum:

```bash
node_archive=$(curl --fail --silent --show-error https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt \
  | awk '/node-v22.*-linux-x64\.tar\.xz$/ { print $2; exit }')
test -n "$node_archive"
curl --fail --silent --show-error --remote-name "https://nodejs.org/dist/latest-v22.x/$node_archive"
curl --fail --silent --show-error --remote-name "https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt"
grep "  $node_archive$" SHASUMS256.txt | sha256sum --check --status -
tar --extract --xz --file "$node_archive" --directory /opt
node_directory=${node_archive%.tar.xz}
ln -sfn "/opt/$node_directory/bin/node" /usr/local/bin/node
ln -sfn "/opt/$node_directory/bin/npm" /usr/local/bin/npm
ln -sfn "/opt/$node_directory/bin/npx" /usr/local/bin/npx
rm -- "$node_archive" SHASUMS256.txt
node --version
```

Install Rust for the runner account and pin the workflow toolchain:

```bash
sudo -u gitea-ci -H sh -c \
  'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.90.0'
sudo -u gitea-ci -H /home/gitea-ci/.cargo/bin/rustup toolchain install 1.90.0 --profile minimal
sudo -u gitea-ci -H /home/gitea-ci/.cargo/bin/rustup component add rustfmt --toolchain 1.90.0
```

Log out and back in (or restart the service) after adding the user to the
`docker` group, then verify as that account:

```bash
sudo -u gitea-ci -H /home/gitea-ci/.cargo/bin/cargo --version
sudo -u gitea-ci -H docker version
node --version
```

### Install and register Gitea Runner

Download the current `gitea-runner` Linux amd64 binary from the official Gitea
Runner release page, verify its published checksum, and install it as
`/usr/local/bin/gitea-runner` with mode `0755`. Then obtain a repository or
organization Actions registration token in Gitea and register it as `gitea-ci`:

```bash
sudo -u gitea-ci -H gitea-runner register --no-interactive \
  --instance https://gitea.example.invalid/ \
  --token REPLACE_WITH_REGISTRATION_TOKEN \
  --name nandunx-release-linux \
  --labels nandunx-release-linux:host
```

Create a systemd unit that starts
`/usr/local/bin/gitea-runner daemon` as `gitea-ci`, with
`HOME=/home/gitea-ci` and `PATH=/home/gitea-ci/.cargo/bin:/usr/local/bin:/usr/bin:/bin`.
For example:

```ini
# /etc/systemd/system/gitea-runner.service
[Unit]
Description=Gitea Actions runner for NANDuNX releases
After=network-online.target docker.service
Wants=network-online.target

[Service]
User=gitea-ci
Group=gitea-ci
WorkingDirectory=/home/gitea-ci
Environment=HOME=/home/gitea-ci
Environment=PATH=/home/gitea-ci/.cargo/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=/usr/local/bin/gitea-runner daemon
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Enable and start it with `systemctl daemon-reload && systemctl enable --now
gitea-runner`, then verify in the Gitea UI that the runner advertises the exact
`nandunx-release-linux` label.

The `host` label is intentional: the release job needs the host Docker daemon
to create the saved image. It gives no job isolation, so do not share this
runner with arbitrary repositories or pull requests. A containerized runner
can be used instead only after providing an equivalent trusted builder image,
Docker socket access and the required tools.

Official references: [Gitea Runner requirements](https://docs.gitea.com/runner/),
[Gitea runner labels](https://docs.gitea.com/runner/labels/),
[Gitea job-token permissions](https://docs.gitea.com/usage/actions/token-permissions/)
and [Tauri Debian packaging](https://v2.tauri.app/distribute/debian/).
