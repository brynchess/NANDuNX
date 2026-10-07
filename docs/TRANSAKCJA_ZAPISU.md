# Transactional restore and resize writes

This document defines the boundary between preflight and block-device writes. It is not a procedure for operating on a real NAND and does not permit bypassing UI confirmation.

## Preconditions

On Linux, the engine accepts only a recognized main physical device node (`/dev/sdX`, `/dev/mmcblkX`, and similar), not a partition, loop device, or ordinary file. Before enabling a write it must:

1. Canonicalize the full target path and compare it with the exact text entered by the user.
2. Reread device parameters without writing and reject read-only, unknown, or changed-capacity targets.
3. Reject a target that is the same block device as the source.
4. Reject the target, its partitions, or active mappings mounted in the current mount namespace.
5. Obtain an exclusive lock on the target descriptor and repeat the mount check afterward.
6. Complete GPT/FAT/keyset preflight and derive an entire immutable relocation plan.

Plain restore may omit a keyset, but none of the other checks. Target selection and confirmation are not persistent configuration.

## Plain restore to a device

The initial device writer restores RAWNAND or explicit FULL NAND to a main device node. It requires an `AuthorizedRestoreTarget` with the full typed path. It does not write BOOT0/BOOT1 when a reader does not expose separate boot nodes.

The writer first copies and rereads all RAWNAND outside the protective MBR and primary GPT, then calls `sync_all`. Only after this succeeds does it write, sync, and verify the initial region through GPT's `first_usable_lba`. Cancellation is accepted only before this last metadata commit; until then, the old primary GPT remains active. Once commit starts, cancellation is ignored and write/sync failures require manual device verification. This sequence does not roll back partition payload data.

eMMC readers are the initial write-hardware scope. A USB reader may appear as `/dev/sdX`; the node name alone does not identify eMMC or broaden permissions. Full path, capacity, and explicit target confirmation remain mandatory. A test on one reader is a controlled scenario, not a recovery guarantee for other adapters.

## Durable operation phases

Sources stay open read-only. The target opens for writing only after preflight and confirmation. Reports contain phase, processed bytes, total bytes, and cancellation state, never a keyset path or key material.

1. Copy the backup to the target and verify written blocks by reading them back. For FULL NAND, only RAWNAND goes to the main device. BOOT0/BOOT1 require separate exposed nodes and separate confirmation; the current writer does not write them.
2. For expansion, relocate encrypted `USER` clusters in descending cluster order: read, decrypt at the old position, re-encrypt for the new position, write, and verify.
3. Write both prepared FAT tables outside the active geometry and verify their decrypted contents.
4. Write and verify backup GPT, then primary GPT.
5. Write and verify backup FAT32 boot sector, then primary boot sector. The last write commits the new geometry.
6. Reread GPT, both boot sectors, and FAT and produce a verification report.

Each required phase ends with a durability flush (`sync_all`) before the next begins. Changing the order requires new fault-injection tests and an update to this document.

## Cancellation and recovery

Cancellation is checked between copy blocks and before each phase. Before the primary FAT32 boot-sector commit, a cancellation or error preserves the old active primary boot sector. Once primary GPT is written, backup metadata may already describe the new range, while the old primary boot sector still describes the old FAT32 geometry and first FAT. Already copied or relocated payload is not rolled back, and backup metadata need not agree before commit.

Fault injection covers partial writes in each phase and `sync_all` errors. A partial pre-commit write preserves the old active geometry. A partial write of the primary boot sector can damage it; the previously verified backup boot sector then contains the new geometry and is the basis for **manual** recovery. A failed `sync_all` stops the transaction before advancing. After the primary boot write, durability is uncertain and requires manual verification.

After primary boot commit, the program verifies the result or reports that manual inspection is needed, including the full target path and last completed phase. It does not automatically roll back. The original backup must be retained; the target is not a substitute.

## Device expansion plan

`restore_and_expand_user` uses the same guarded target authorization as plain restore through the privileged adapter. Before the first write, it revalidates the source image, BIS Key 2, target capacity, both FAT32 metadata copies, and a complete relocation plan from the locked read-only source. After opening the target for writing, it rechecks target identity and mounts. It copies and verifies RAWNAND, then executes the already prepared expansion plan. A source-planning failure therefore cannot leave another partially restored target.

Sync points and recovery follow the durable phase order: relocation, both FATs, backup GPT, primary GPT, backup boot sector, and finally primary boot sector. Cancellation is allowed between relocations and before these phases, but not after final commit begins. An error before that point keeps the old primary boot geometry active; afterward, the program only verifies and reports manual-inspection needs.

## In-place `USER` expansion

In-place expansion is a separate operation, not a shortcut around restore. It reads and modifies a physically larger existing RAWNAND area, accepts no backup as the input image, and does not write BOOT0/BOOT1. A separate backup must be retained on another device.

Before opening the device for writing, it read-only reidentifies the main device, checks capacity and mounts, validates primary GPT/`USER`, verifies BIS Key 2 against primary and backup boot sectors, compares FAT copies, and builds the complete relocation plan. It also requires exact canonical target-path confirmation and an exclusive lock.

Cluster ciphertext cannot simply be copied. AES-128-XTS uses a 16 KiB unit number relative to the partition as a tweak. FAT growth shifts the data area and changes that number. Each allocated cluster is read at its old index, decrypted, re-encrypted at its new index, and read back for verification. `USER` retains its starting LBA, 16 KiB cluster size, and BIS key. Descending relocation avoids overwriting unread source clusters.

The plan, phase order, and `sync_all` points match restore+resize, except no RAWNAND copy: relocation, both FATs, backup GPT, primary GPT, backup boot, and primary boot. Cancellation is allowed until the final phase starts. There is no automatic rollback; hardware trials require an independent retained backup and must never use the only copy of data.

## Fixtures, adapters, and limits of evidence

The safe target barrier includes identification, mount checks, locking, and exact confirmation. An ordinary-file-only restore fixture copies RAWNAND in 16 KiB blocks, calls a progress observer at block boundaries, syncs, and verifies byte for byte. FULL NAND skips the two 4 MiB boot areas, matching the main eMMC node's address space. Cancellation stops on a block boundary and reports written bytes, without claiming verification or recovery of the partly overwritten fixture. Fixture APIs explicitly reject block devices.

The durable ordinary-file restore+resize fixture checks source GPT/FAT and BIS key before writing; after verified RAWNAND copy it relocates clusters, writes FATs, both GPTs, and both boot sectors. Every completed phase is reread and ends with `sync_all`. Checkpoints before relocation, FAT, backup/primary GPT, and backup/primary boot show that primary boot geometry changes only at the last phase. The engine buffers at most a 16 KiB unit, FAT, and GPT metadata, not the entire output image. After full commit it rereads GPT and encrypted `USER` metadata.

The fixture also simulates a restart just before each of six `sync_all` calls by discarding writes since the last successful sync. An independent read-only parser then classifies both GPT copies and the primary/backup boot sectors discoverable in the fixture's first encrypted `USER` unit. It distinguishes old active geometry, completed commit, a need for manual recovery from backup boot, and indeterminate state. It does not repair anything.

Privileged Web and desktop adapters call the same public core writers for all three modes. In-place Web preflight runs again with keyset immediately before target locking; the writer rebuilds the plan under the lock and again on the writable descriptor. Tauri resolves session file references and the current device, repeats complete preflight, checks typed path, and obtains a read-only target lock before handing it to a worker. Preflight and writing run off the GUI thread. Each job has a session ID, structured status/progress, and a cancellation request checked at core boundaries. Only one desktop operation, including preparation, may run at once. Window closing is blocked until the worker ends.

A fixture cache model cannot reproduce a particular driver, controller, or arbitrary power loss during real I/O. An interrupted plain restore can leave a partially overwritten target; a final commit error can require independent reads or manual recovery. The first expansion on hardware remains a controlled test with an original backup and independent later verification.

## Windows protected-handle lifecycle (W4/W5)

Windows needs its own authorization; a Linux `fs2` lock is not a Win32 volume lock. `protect_windows_disk` takes a session snapshot, all source paths, and the exact `\\.\PhysicalDriveN` confirmation:

1. Repeat read-only checks of disk identity, geometry, PnP/serial/GPT IDs, system and pagefile status, every volume extent, and every source.
2. For each GUID volume, open with `GENERIC_READ | GENERIC_WRITE` and `FILE_SHARE_READ | FILE_SHARE_WRITE`, call `FSCTL_LOCK_VOLUME`, and keep every handle. Check extents immediately before each lock. A failed lock stops the operation. A volume-free disk uses a separate path: exclusive unshared physical handle, identity recheck, proof that a second handle fails with `ERROR_SHARING_VIOLATION`, and complete volume re-enumeration. Any newly visible volume, ambiguous refusal, or enumeration error stops before writing.
3. Only after all locks, call `FSCTL_DISMOUNT_VOLUME` while keeping the handles. Open the physical disk with `CreateFileW`, `GENERIC_READ | GENERIC_WRITE`, no sharing, and `FILE_FLAG_WRITE_THROUGH`. It does not use `FILE_FLAG_NO_BUFFERING`, so buffer-memory alignment is not assumed. Recheck the handle's number, length, sectors, GPT ID, and enumerated PnP/serial. Close all handles on any failure.
4. `File::sync_all` on that same physical handle is the intended phase flush. Driver behavior, 512e, and durability after disconnection require disposable VHD and then test-reader trials. Fixture sync success does not prove eMMC durability.

Protected writable handles stay inside core. Public `authorize_windows_restore` and `authorize_windows_in_place` sessions retain target and volume handles plus open source files and keyset. Split backup parts allow read sharing only, preventing modification or rename during operation. Restore+resize validates GPT, key, both boot sectors, both FAT copies, complete chains, and relocation plan before opening the target for writing. In-place plans read-only and repeats the plan on the protected handle before writing.

Physical-disk metadata reads use whole logical sectors. A 1408-byte GPT table occupies three 512-byte sectors; parsing and verification use only its declared 1408 bytes. Recovery reads follow the same rule. Win32 error 87 on this GPT read was observed on the test reader; physical revalidation remains required.

Per the user's 2026-10-05 decision, W5 desktop is one `requireAdministrator` EXE. UAC appears before the GUI; refusal ends launch before opening the target. Authorization and writing run on a worker thread in the same process through public core sessions. Each session repeats preflight and compares selected snapshot, full typed path, sources, and target. Elevation alone never authorizes a write. The window blocks closing during an operation; GUI cancellation is checked between blocks and before commit. After primary boot or primary metadata commit starts, the worker completes verification if I/O works or reports an uncertain result without rollback. Forced process termination or power loss requires independent device inspection before retry. One log beside the EXE records phases and progress, without keys or backup contents. After restart, `inspect_windows_recovery` reidentifies the disk and reads both GPTs, boot sectors, and FAT without writing or repairing. Tests without physical eMMC do not prove controller behavior under power loss.
