# NANDuNX on TrueNAS SCALE

This file is shipped with the Docker release bundle. It applies to TrueNAS
SCALE 24.10 and newer, whose Apps backend uses Docker. NANDuNX is deliberately
not a LAN service: its web interface binds only to `127.0.0.1` and can write a
passed-through block device after guarded preflight and an exact confirmation.

## Load the released Docker bundle

Extract the `nandunx-docker-<version>-linux-x86_64.tar.gz` release asset on the
TrueNAS host and work from the extracted directory:

```bash
tar -xzf nandunx-docker-<version>-linux-x86_64.tar.gz
cd nandunx-docker-<version>-linux-x86_64
gzip -dc nandunx-web-image-<version>.tar.gz | docker load
cp .env.example .env
```

Edit `.env` before starting anything. Set `NANDUNX_TAG` to the version loaded
above, choose a private state directory in a dedicated TrueNAS dataset, and
set `NANDUNX_DEVICE_PATH` to exactly one currently connected **whole** USB block
device. Do not use a partition, a TrueNAS pool disk, or a system disk. USB
names such as `/dev/sdX` may change after reconnecting a reader; stop the App,
recheck it, edit the configuration and redeploy after each reconnect.

Create the state directory with owner root and mode `0700`. It must have free
space for the uploaded backup. It contains only the current upload session and
a redacted last-run log; it is not a place for permanent backups or keys.

## Recommended: Apps → Install via YAML

In TrueNAS, choose **Apps → Discover Apps → menu → Install via YAML**, name
the App `nandunx`, and paste this after replacing the `CHANGE` placeholders and the image
version. Do not add a Portal or port forwarding: host networking preserves the
loopback boundary and port forwarding is disabled for it.

```yaml
name: nandunx
services:
  nandunx-web:
    image: nandunx-web:CHANGE-VERSION
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
      - /dev/sdX:/dev/sdX:rwm # CHANGE: one currently connected whole disk
    volumes:
      - /mnt/CHANGE-POOL/apps/nandunx/state:/var/lib/nandunx
      # Required: lets NANDuNX reject devices mounted in the host namespace.
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

The `devices:` entry is the only device permission NANDuNX needs: it passes one
node and its Docker cgroup `rwm` rule. Do not use `privileged: true`, `/dev:/dev`,
`SYS_ADMIN`, or `SYS_RAWIO`. The container uses root only to open the selected
device node; all Linux capabilities are dropped.

The read-only `/proc/1/mountinfo` mount is mandatory. A container normally has
its own mount namespace and therefore cannot otherwise detect a disk mounted by
the TrueNAS host. Removing it weakens the destructive-write preflight.

The current guided **Custom App** form does not offer Compose-equivalent,
single-device passthrough. Its Privileged checkbox exposes all host devices and
is not an acceptable replacement. Use Install via YAML for NANDuNX write
operations.

## Access and checks

From another machine, access the loopback-only UI through SSH:

```bash
ssh -N -L 4321:127.0.0.1:4321 admin@truenas-host
```

Open `http://127.0.0.1:4321` locally. On the TrueNAS shell, check the service
without exposing it on the LAN:

```bash
curl --fail http://127.0.0.1:4321/api/v1/status
```

For the complete Docker and TrueNAS explanation, see `docs/DOCKER.md` in the
source repository.
