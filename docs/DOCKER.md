# Docker and TrueNAS SCALE

The NANDuNX container is for Linux with Docker Engine. It keeps the same safety boundary as the systemd service: HTTP binds only to `127.0.0.1`, and the container receives `rwm` access to exactly **one** explicitly selected whole block device. Do not use `privileged: true` in normal Compose.

## Docker Compose

Docker Engine with Compose v2 is required. `NANDUNX_STATE_DIR` needs at least enough free space for the uploaded backup. It is private temporary session storage and holds one redacted log, not permanent backups or keysets.

```bash
cp .env.example .env
# Edit .env: set an existing private NANDUNX_STATE_DIR and the currently
# connected whole device as NANDUNX_DEVICE_PATH.
install -d -m 0700 /var/lib/nandunx-container
docker compose config
docker compose build
docker compose up -d
curl --fail http://127.0.0.1:4321/api/v1/status
```

`docker-compose.yml` deliberately uses `network_mode: host` instead of `ports:`. Binding to `127.0.0.1:4321` inside the container then uses the host loopback. Do not set `NANDUNX_BIND=0.0.0.0:4321` or add an unauthenticated reverse proxy: the Web UI can write a block device.

From another computer, use an SSH tunnel instead of exposing a LAN port:

```bash
ssh -N -L 4321:127.0.0.1:4321 admin@docker-host
```

Open `http://127.0.0.1:4321` on the client. For container diagnostics:

```bash
docker compose ps
docker compose logs -f nandunx-web
tail -f /var/lib/nandunx-container/last-run.log
```

Before every `up`, check the connected reader and its capacity. A USB `/dev/sdX` name may change after reconnection. Do not select a partition (`/dev/sdb1`), a TrueNAS pool/system disk, or a device reconnected since container startup. After changing the reader, stop the container, check `.env`, and restart it.

### Why `mountinfo` is mounted

A container has its own mount namespace, so `/proc/self/mountinfo` does not show host mounts. NANDuNX must detect if the selected disk or one of its partitions is mounted on the host. Compose therefore bind-mounts only the host's `/proc/1/mountinfo` read-only and passes it through `NANDUNX_MOUNTINFO_PATH`. Do not remove this mount or variable: without it, container preflight cannot enforce the host-mount write barrier.

### USB and block-device permissions

Standard Docker Compose needs no extra capability or privileged mode. `devices:` both passes the device node and grants its cgroup `rwm` rule. The container uses UID 0 only to open that node; `cap_drop: [ALL]`, `no-new-privileges`, and a read-only root filesystem remain active. NANDuNX does not mount disks or call `sudo`, and it needs neither `SYS_ADMIN`, `SYS_RAWIO`, nor all of `/dev`.

Do not replace `devices:` with `/dev:/dev` or add `privileged: true` to work around an access failure. If a correctly passed device cannot be opened, stop and check whether the host uses the disk or Docker/TrueNAS policy blocks it.

## TrueNAS SCALE 24.10+

SCALE Apps use Docker from 24.10 onward. **Install via YAML** accepts Docker Compose syntax. UI details may vary by SCALE release; these steps apply to the Docker-backed Apps, not the old Kubernetes-backed 24.04 and earlier.

### Recommended: Apps → Install via YAML

This route can restrict access to one device node with `devices:`. First build the image on the TrueNAS host from this checkout, or use your own trusted registry:

```bash
docker compose build
docker image inspect nandunx-web:local
```

Create a dataset such as `tank/apps/nandunx` with a private `state` directory: no SMB/NFS or shared ACL, owner root, mode `0700`. In Apps select **Discover Apps → menu → Install via YAML**, name the App `nandunx`, and paste this after replacing the device path and dataset path:

```yaml
name: nandunx
services:
  nandunx-web:
    image: nandunx-web:local
    restart: unless-stopped
    network_mode: host
    user: "0:0"
    environment:
      NANDUNX_BIND: "127.0.0.1:4321"
      NANDUNX_WEB_ROOT: /opt/nandunx/web
      NANDUNX_ARTIFACT_DIR: /var/lib/nandunx/uploads
      NANDUNX_LAST_RUN_LOG: /var/lib/nandunx/last-run.log
      NANDUNX_MOUNTINFO_PATH: /run/nandunx/host-mountinfo
    devices:
      # Replace with exactly one currently connected whole USB disk.
      - /dev/sdX:/dev/sdX:rwm
    volumes:
      # Replace with a private directory in a separate dataset.
      - /mnt/tank/apps/nandunx/state:/var/lib/nandunx
      # Keep read-only; this is needed to reject mounted targets.
      - /proc/1/mountinfo:/run/nandunx/host-mountinfo:ro
      - /sys:/sys:ro
    read_only: true
    cap_drop:
      - ALL
    security_opt:
      - no-new-privileges:true
    tmpfs:
      - /tmp:rw,noexec,nosuid,size=64m,mode=1777
    init: true
```

Do not add port mapping or an Apps Portal. With `network_mode: host`, port mapping is disabled and a proper loopback bind is unreachable from the NAS LAN address. Use the SSH tunnel above. Check the App and container logs, then check `curl --fail http://127.0.0.1:4321/api/v1/status` in the TrueNAS shell.

After updating an image, rebuild it with `docker compose build`, stop and restart the App, and check the status endpoint. The App cannot automatically gain access to a new USB device; edit `devices:` deliberately and redeploy after each change.

### Guided Custom App form

**Apps → Discover Apps → Custom App** can test image startup and storage/network settings, but the current guided screen has no field equivalent to Compose `devices:`. TrueNAS documentation says containers cannot see host devices by default, while **Privileged** grants broad host access to all devices. For NANDuNX writes, use the YAML route above.

For a startup-only test without device operations, set image `nandunx-web:local`, user `0:0`, Host Network enabled, a state dataset host path to `/var/lib/nandunx`, and the five environment variables in the YAML example (`NANDUNX_BIND`, `NANDUNX_WEB_ROOT`, `NANDUNX_ARTIFACT_DIR`, `NANDUNX_LAST_RUN_LOG`, `NANDUNX_MOUNTINFO_PATH`). Do not enable **Privileged**.

If a future form allows both single-device passthrough and read-only host paths for `/proc/1/mountinfo` and `/sys`, it may reproduce the YAML configuration. Otherwise, do not replace it with **Privileged**.

Official TrueNAS references: [Custom App Screens](https://www.truenas.com/docs/scale/26/apps/installcustomappscreens/), [Installing Custom Apps](https://apps.truenas.com/managing-apps/installing-custom-apps/), and [App Storage](https://apps.truenas.com/getting-started/app-storage/).
