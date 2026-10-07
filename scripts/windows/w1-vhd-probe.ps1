$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class NandunxW1Native {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern SafeFileHandle CreateFile(
        string name, uint access, uint share, IntPtr security,
        uint creation, uint flags, IntPtr templateFile);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool DeviceIoControl(
        SafeFileHandle handle, uint code, IntPtr input, uint inputSize,
        IntPtr output, uint outputSize, out uint returned, IntPtr overlapped);
}
'@

function Assert-OwnVhdDisk {
    param([string]$ImagePath, [int]$DiskNumber = -1)
    $image = Get-DiskImage -ImagePath $ImagePath -ErrorAction Stop
    if (-not $image.Attached) { throw 'The new VHDX is not attached' }
    $disks = @($image | Get-Disk -ErrorAction Stop)
    if ($disks.Count -ne 1) { throw 'The VHDX must resolve to exactly one disk' }
    $disk = $disks[0]
    if ($disk.BusType -ne 'File Backed Virtual' -or $disk.IsBoot -or $disk.IsSystem) {
        throw 'The image is not an isolated file-backed virtual disk'
    }
    if ($DiskNumber -ge 0 -and $disk.Number -ne $DiskNumber) {
        throw 'The VHDX disk number changed during the probe'
    }
    return $disk
}

function Open-Direct {
    param([string]$Path, [uint32]$Access, [uint32]$Share)
    $handle = [NandunxW1Native]::CreateFile(
        $Path, $Access, $Share, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)
    return @{ Handle = $handle; Error = [Runtime.InteropServices.Marshal]::GetLastWin32Error() }
}

$id = [Guid]::NewGuid().ToString('N')
$vhdPath = Join-Path $env:TEMP "nandunx-w1-$id.vhdx"
$diskpartPath = Join-Path $env:TEMP "nandunx-w1-$id.diskpart.txt"
$reportPath = Join-Path $env:TEMP "nandunx-w1-$id.json"
if (Test-Path -LiteralPath $vhdPath) { throw 'Refusing to replace an existing image' }
$result = [ordered]@{
    vhd_path = $vhdPath
    report_path = $reportPath
    created_utc = [DateTime]::UtcNow.ToString('u')
    os = [Environment]::OSVersion.VersionString
}
$attached = $false
try {
    @("create vdisk file=`"$vhdPath`" maximum=64 type=expandable") |
        Set-Content -LiteralPath $diskpartPath -Encoding ASCII
    & diskpart.exe /s $diskpartPath | Out-Host
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $vhdPath)) {
        throw 'DiskPart did not create the new VHDX'
    }
    $image = Mount-DiskImage -ImagePath $vhdPath -NoDriveLetter -PassThru -ErrorAction Stop
    $attached = $true
    $disk = Assert-OwnVhdDisk $vhdPath
    $number = [int]$disk.Number
    $result.disk = [ordered]@{
        number = $number
        model = $disk.FriendlyName
        serial = $disk.SerialNumber
        unique_id = $disk.UniqueId
        size = [uint64]$disk.Size
        logical_sector = [uint32]$disk.LogicalSectorSize
        physical_sector = [uint32]$disk.PhysicalSectorSize
        bus = [string]$disk.BusType
    }

    $rawPath = "\\.\PhysicalDrive$number"
    $rawFirst = Open-Direct $rawPath 2147483648 0
    $result.raw_exclusive_open = -not $rawFirst.Handle.IsInvalid
    $result.raw_exclusive_error = $rawFirst.Error
    try {
        if (-not $rawFirst.Handle.IsInvalid) {
            $rawSecond = Open-Direct $rawPath 2147483648 3
            $result.raw_second_open = -not $rawSecond.Handle.IsInvalid
            $result.raw_second_error = $rawSecond.Error
            $rawSecond.Handle.Dispose()
        }
    } finally {
        $rawFirst.Handle.Dispose()
    }

    $disk = Assert-OwnVhdDisk $vhdPath $number
    $disk | Initialize-Disk -PartitionStyle GPT -PassThru -ErrorAction Stop | Out-Null
    $disk = Assert-OwnVhdDisk $vhdPath $number
    $partition = $disk | New-Partition -UseMaximumSize -AssignDriveLetter -ErrorAction Stop
    $disk = Assert-OwnVhdDisk $vhdPath $number
    $partition | Format-Volume -FileSystem NTFS -Force -Confirm:$false -ErrorAction Stop | Out-Null
    if (-not $partition.DriveLetter) { throw 'The VHDX partition has no drive letter' }

    $volumePath = "\\.\$($partition.DriveLetter):"
    $volume = Open-Direct $volumePath 3221225472 3
    $result.volume_open = -not $volume.Handle.IsInvalid
    $result.volume_open_error = $volume.Error
    try {
        if (-not $volume.Handle.IsInvalid) {
            [uint32]$returned = 0
            $locked = [NandunxW1Native]::DeviceIoControl(
                $volume.Handle, 0x00090018, [IntPtr]::Zero, 0,
                [IntPtr]::Zero, 0, [ref]$returned, [IntPtr]::Zero)
            $result.volume_lock = $locked
            $result.volume_lock_error = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
            if ($locked) {
                $volumeSecond = Open-Direct $volumePath 2147483648 3
                $result.volume_second_open = -not $volumeSecond.Handle.IsInvalid
                $result.volume_second_error = $volumeSecond.Error
                $volumeSecond.Handle.Dispose()
            }
        }
    } finally {
        $volume.Handle.Dispose()
    }
} catch {
    $result.error = $_.Exception.Message
    throw
} finally {
    if ($attached) {
        try {
            Dismount-DiskImage -ImagePath $vhdPath -ErrorAction Stop
            $result.detached = $true
        } catch {
            $result.detached = $false
            $result.detach_error = $_.Exception.Message
        }
    }
    Remove-Item -LiteralPath $diskpartPath -ErrorAction SilentlyContinue
    $result | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $reportPath -Encoding UTF8
    Write-Host "VHDX: $vhdPath"
    Write-Host "Report: $reportPath"
    if ($result.detached -eq $false) {
        Write-Error 'VHDX could not be detached; inspect the report before further tests'
    }
}
