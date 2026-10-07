# NANDuNX

NANDuNX helps restore your own Nintendo Switch NAND backup to a larger storage device and automatically expand the `USER` partition while preserving its data. The Linux edition runs locally as a desktop app or a Web UI. A Windows desktop build is also available; broader hardware validation and release packaging remain in progress.

## Why NANDuNX

[NxNandManager](https://github.com/eliboa/NxNandManager) inspired this project. NANDuNX focuses on making one of its tasks easier: restoring a backup to a larger device and using the extra space for `USER` without formatting that partition. NANDuNX calculates the target size, shows an operation plan and read-only checks before writing, and reports progress. It can also expand `USER` on a device that already contains the backup.

## Operations

- **RAWNAND restore:** copies a backup to the selected device. No keyset is needed.
- **Restore and expand `USER`:** restores a backup and uses the additional device capacity. Requires BIS Key 2 from your own keyset.
- **Expand `USER` in place:** works on an existing device and requires a separate, retained backup and BIS Key 2.

NANDuNX accepts a single RAWNAND file or a split backup. It also recognizes an explicit FULL NAND image containing BOOT0, BOOT1, then RAWNAND. NANDuNX does not write BOOT0 or BOOT1 to the device; restore them separately with Hekate when needed.

## Before writing

1. Keep an independent backup and check that you selected the intended device. Writing replaces its existing contents.
2. Select the operation, backup file or parts, and the whole target device. Select your keyset for either expansion mode.
3. Review the plan and run the read-only preflight. NANDuNX checks the partition layout, source, target, and device availability.
4. Type the full target-device path to confirm the write. NANDuNX repeats its checks immediately before starting.
5. Watch progress and leave the device connected until the operation ends. Cancellation takes effect at safe boundaries before the final metadata commit.

The interface defaults to English and includes Polish as a selectable, remembered language. Its translations are isolated from write logic so additional languages can be added without changing the NAND workflow. The three views are **Preparation**, **Plan and checks**, and **Execution**. If a write is interrupted, inspect the report and device before trying again. Use your retained backup for recovery. Check the resulting data on the console after completion.

## Editions and installation

- **Linux desktop:** install the `.deb` package from a project release. The operating system must grant access to the block device; NANDuNX does not run `sudo`.
- **Linux headless:** a local Web UI can run as a systemd service. It listens only on `127.0.0.1:4321`. Use an SSH tunnel from another computer. See [development and source-build instructions](docs/DEVELOPMENT.md).
- **Docker / TrueNAS SCALE:** follow the [deployment guide](docs/DOCKER.md). Its configuration passes through exactly one whole target device.
- **Windows desktop preview:** tagged releases include an unsigned NSIS `.exe` installer with the same three operations and administrator access through UAC. Use it only with an independent backup and a device you are authorized to modify. Broader reader, recovery, signing, and hardware validation remain open; see the [roadmap](ROADMAP.md) and [Windows test report](docs/W6-RAPORT.md).

The Web UI uploads selected files to a private session directory on the machine running NANDuNX. The desktop edition uses files in their existing locations. Do not put backups or keysets in the project directory, and use only data and devices you are authorized to access.

## License and further information

NANDuNX is available under **GNU GPL version 3 or later**; see [LICENSE](LICENSE).

The [roadmap](ROADMAP.md) tracks current status and support limits. Technical assumptions and limitations are in [docs/ZAŁOŻENIA.md](docs/ZAŁOŻENIA.md).
