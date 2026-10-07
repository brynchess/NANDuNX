# Lock and flush probe against a freshly created, disposable VHDX.
# This script changes only its own VHDX and never writes NAND sectors.
param([switch]$Resize, [switch]$Cancel)
$ErrorActionPreference = 'Stop'
if ($Resize -and $Cancel) { throw 'Select one VHDX probe mode' }
$env:Path = "$env:USERPROFILE\.cargo\bin;" + [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
$project = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$id = [Guid]::NewGuid().ToString('N')
$imagePath = Join-Path $env:TEMP "nandunx-w4-$id.vhdx"
$diskpartPath = Join-Path $env:TEMP "nandunx-w4-$id.diskpart.txt"
$attached = $false
try {
    "create vdisk file=`"$imagePath`" maximum=64 type=expandable" |
        Set-Content -LiteralPath $diskpartPath -Encoding ASCII
    & diskpart.exe /s $diskpartPath | Out-Null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $imagePath)) {
        throw 'Disposable VHDX creation failed'
    }
    Mount-DiskImage -ImagePath $imagePath -NoDriveLetter -ErrorAction Stop | Out-Null
    $attached = $true
    $disk = @(Get-DiskImage -ImagePath $imagePath | Get-Disk)
    if ($disk.Count -ne 1 -or $disk[0].BusType -ne 'File Backed Virtual' -or
        $disk[0].IsBoot -or $disk[0].IsSystem) {
        throw 'VHDX does not resolve to one isolated virtual disk'
    }
    $number = [int]$disk[0].Number
    $disk[0] | Initialize-Disk -PartitionStyle GPT -PassThru | Out-Null
    $disk = @(Get-DiskImage -ImagePath $imagePath | Get-Disk)
    if ($disk.Count -ne 1 -or [int]$disk[0].Number -ne $number -or
        $disk[0].BusType -ne 'File Backed Virtual') {
        throw 'VHDX disk identity changed before partition creation'
    }
    $disk[0] | New-Partition -UseMaximumSize | Out-Null
    Set-Location $project
    if ($Resize) {
        $env:NANDUNX_W4_VHD_RESIZE_DISK = [string]$number
        & cargo test -p nandunx-core --locked disposable_vhd_in_place_resize_when_requested -- --nocapture
    } elseif ($Cancel) {
        $env:NANDUNX_W4_VHD_CANCEL_DISK = [string]$number
        & cargo test -p nandunx-core --locked disposable_vhd_cancel_before_primary_commit_when_requested -- --nocapture
    } else {
        $env:NANDUNX_W4_VHD_DISK = [string]$number
        & cargo test -p nandunx-core --locked disposable_vhd_lock_and_flush_probe_when_requested -- --nocapture
    }
    if ($LASTEXITCODE -ne 0) { throw 'VHDX lock and flush probe failed' }
    if ($Resize) {
        Write-Host 'PASS disposable VHDX restore and USER resize with GPT/FAT readback'
    } elseif ($Cancel) {
        Write-Host 'PASS disposable VHDX cancellation before primary metadata commit'
    } else {
        Write-Host 'PASS disposable VHDX lock, synthetic RAWNAND restore, flush and reopen'
    }
} finally {
    Remove-Item -LiteralPath $diskpartPath -ErrorAction SilentlyContinue
    if ($attached) {
        Dismount-DiskImage -ImagePath $imagePath -ErrorAction Stop | Out-Null
    }
    Remove-Item -LiteralPath $imagePath -ErrorAction SilentlyContinue
}
