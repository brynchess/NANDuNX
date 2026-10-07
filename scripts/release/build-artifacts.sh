#!/usr/bin/env bash
# Builds the release assets used by both GitHub Actions and Gitea Actions.
# It intentionally publishes nothing: the caller owns release credentials.
set -euo pipefail

readonly tag=${1:?usage: build-artifacts.sh vX.Y.Z}
readonly version=${tag#v}
readonly release_directory="$(pwd -P)/release"

[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] \
    || { printf 'error: expected a version tag such as v0.1.0\n' >&2; exit 1; }

read_json_version() {
    node --input-type=module --eval "import config from './$1' with { type: 'json' }; process.stdout.write(config.version);"
}

cargo_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
package_version=$(read_json_version package.json)
tauri_version=$(read_json_version src-tauri/tauri.conf.json)
[[ -n $cargo_version && $cargo_version == "$package_version" && $cargo_version == "$tauri_version" ]] \
    || { printf 'error: Cargo, package and Tauri versions must match\n' >&2; exit 1; }
[[ $version == "$cargo_version" ]] \
    || { printf 'error: tag %s does not match project version %s\n' "$tag" "$cargo_version" >&2; exit 1; }

rm -rf -- "$release_directory"
mkdir -p "$release_directory"

npm ci
npm run build:web
cargo fmt --check
cargo test --workspace

# Desktop package (amd64, built by the Linux x86_64 runner).
npm run tauri build -- --bundles deb
shopt -s nullglob
debian_packages=(target/release/bundle/deb/*.deb)
(( ${#debian_packages[@]} == 1 )) \
    || { printf 'error: expected exactly one .deb artifact\n' >&2; exit 1; }
cp -- "${debian_packages[0]}" "$release_directory/nandunx_${version}_amd64.deb"

# Headless web bundle: runnable without Node/npm after extraction.
cargo build --locked --release -p nandunx-web
readonly web_bundle_root="$release_directory/nandunx-web-${version}-linux-x86_64"
install -d -m 0755 "$web_bundle_root"
install -m 0755 target/release/nandunx-web "$web_bundle_root/nandunx-web"
cp -a dist "$web_bundle_root/web"
install -m 0644 README.md "$web_bundle_root/README.md"
install -m 0644 packaging/systemd/nandunx-web.service "$web_bundle_root/nandunx-web.service"
install -m 0755 scripts/install-headless.sh "$web_bundle_root/install-headless.sh"
tar --numeric-owner --owner=0 --group=0 -C "$release_directory" \
    -czf "$release_directory/nandunx-web-${version}-linux-x86_64.tar.gz" \
    "$(basename "$web_bundle_root")"
rm -rf -- "$web_bundle_root"

# Docker bundle: users load this exact image, then use the bundled Compose
# configuration. Docker is required on the CI runner for this release type.
docker build --tag "nandunx-web:${version}" .
readonly docker_bundle_root="$release_directory/nandunx-docker-${version}-linux-x86_64"
install -d -m 0755 "$docker_bundle_root"
docker save "nandunx-web:${version}" | gzip -n > "$docker_bundle_root/nandunx-web-image-${version}.tar.gz"
install -m 0644 docker-compose.yml "$docker_bundle_root/docker-compose.yml"
install -m 0644 .env.example "$docker_bundle_root/.env.example"
install -m 0644 Dockerfile "$docker_bundle_root/Dockerfile"
install -m 0644 README-TRUENAS.md "$docker_bundle_root/README-TRUENAS.md"
tar --numeric-owner --owner=0 --group=0 -C "$release_directory" \
    -czf "$release_directory/nandunx-docker-${version}-linux-x86_64.tar.gz" \
    "$(basename "$docker_bundle_root")"
rm -rf -- "$docker_bundle_root"

(cd "$release_directory" && sha256sum -- \
    nandunx_*.deb \
    nandunx-web-*.tar.gz \
    nandunx-docker-*.tar.gz > SHA256SUMS)
