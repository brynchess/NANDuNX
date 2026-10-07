#!/usr/bin/env bash
# Installs the already-built loopback-only NANDuNX Web UI as a systemd service.
# Run this script as root; it deliberately never invokes sudo itself.
set -euo pipefail

readonly INSTALL_ROOT="/usr/local"
readonly BINARY_DESTINATION="${INSTALL_ROOT}/lib/nandunx/nandunx-web"
readonly WEB_DESTINATION="${INSTALL_ROOT}/share/nandunx/web"
readonly UNIT_DESTINATION="/etc/systemd/system/nandunx-web.service"

usage() {
    cat <<'EOF'
Usage: install-headless.sh --binary PATH --web-root PATH [--no-start]

Install an already-built NANDuNX Web UI as the nandunx-web systemd service.

  --binary PATH    Executable built with: cargo build --release -p nandunx-web
  --web-root PATH  Directory built with:  npm run build:web
  --no-start       Install and enable the unit, but do not start it now
  -h, --help       Show this help

The service listens only on 127.0.0.1:4321 and runs as root so it can access
block devices without per-device ACL setup. It supports guarded RAWNAND
restore, restore+resize and resize in-place of USER; it does not write
BOOT0/BOOT1.
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

binary=""
web_root=""
start_service=true

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)
            [[ $# -ge 2 ]] || fail "--binary requires a path"
            binary=$2
            shift 2
            ;;
        --web-root)
            [[ $# -ge 2 ]] || fail "--web-root requires a path"
            web_root=$2
            shift 2
            ;;
        --no-start)
            start_service=false
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown option: $1"
            ;;
    esac
done

[[ $EUID -eq 0 ]] || fail "run this installer as root (for example: sudo $0 ...)"
[[ -n $binary ]] || fail "--binary is required"
[[ -n $web_root ]] || fail "--web-root is required"
[[ -x $binary && -f $binary ]] || fail "binary is not an executable regular file: $binary"
[[ -d $web_root && -f $web_root/index.html ]] || fail "web root must contain index.html: $web_root"
command -v systemctl >/dev/null || fail "systemctl is required"
[[ -d /run/systemd/system ]] || fail "a running systemd system manager is required"

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
unit_source="${script_dir}/../packaging/systemd/nandunx-web.service"
[[ -f $unit_source ]] || fail "missing unit template: $unit_source"

install -d -o root -g root -m 0755 "${INSTALL_ROOT}/lib/nandunx" "$WEB_DESTINATION"
install -o root -g root -m 0755 "$binary" "$BINARY_DESTINATION"
cp -a "$web_root"/. "$WEB_DESTINATION"/
chown -R root:root "$WEB_DESTINATION"
find "$WEB_DESTINATION" -type d -exec chmod 0755 {} +
find "$WEB_DESTINATION" -type f -exec chmod 0644 {} +
install -o root -g root -m 0644 "$unit_source" "$UNIT_DESTINATION"

systemctl daemon-reload
systemctl enable nandunx-web.service >/dev/null

if [[ $start_service == true ]]; then
    systemctl restart nandunx-web.service
    systemctl --quiet is-active nandunx-web.service || fail "service did not become active; inspect: journalctl -u nandunx-web.service"
fi

printf '%s\n' "NANDuNX Web UI installed."
printf '%s\n' "Local URL: http://127.0.0.1:4321"
printf '%s\n' "Status:    systemctl status nandunx-web.service"
printf '%s\n' "Logs:      journalctl -u nandunx-web.service"
printf '%s\n' "Last run:  sudo tail -f /var/lib/nandunx-web/last-run.log"
