# W1 — VHD probe plan

This probe writes only to a new, disposable VHDX file in the Windows temporary directory. It uses no NAND backup, keys, or physical reader. The script must fail if it cannot link the selected `PhysicalDrive` number to the exact VHDX path it created.

1. Generate a random filename, verify it does not exist, create a VHDX with DiskPart, and attach it. Rediscover the disk through `Get-DiskImage -ImagePath ... | Get-Disk` and require type `File Backed Virtual`.
2. Read model, capacity, sectors, disk number, and identifiers. Open the resulting `\\.\PhysicalDriveN` read-only. Check whether a second handle can open it while the first disallows sharing. This is a prototype observation, not proof of RAW protection.
3. Create one test NTFS partition on **that same verified VHDX**. Open its volume directly, call `FSCTL_LOCK_VOLUME`, confirm that concurrent opening is denied, and release the handle. Never dismount a real volume.
4. Detach the VHDX in a `finally` block. Retain the file and log for read-only analysis; the script prints the path. If detaching fails, stop further tests and detach manually with `Dismount-DiskImage` after checking the exact file path. Do not remove host volumes or the system disk.

The result describes only this Windows/VHDX driver combination. It does not prove that a volume-free eMMC reader can be locked safely, that flushes are durable, or that the Windows writer can be enabled. Without unambiguous RAW protection, writes remain blocked.
