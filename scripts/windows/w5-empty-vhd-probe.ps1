# Exercises only the exclusive RAW handle on a disposable GPT VHDX without volumes.
$ErrorActionPreference = 'Stop'
$env:Path = "$env:USERPROFILE\.cargo\bin;" +
    [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
    [Environment]::GetEnvironmentVariable('Path', 'User')
$project = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$id = [Guid]::NewGuid().ToString('N')
$imagePath = Join-Path $env:TEMP "nandunx-w5-empty-$id.vhdx"
$diskpartPath = Join-Path $env:TEMP "nandunx-w5-empty-$id.diskpart.txt"
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
        $disk[0].IsBoot -or $disk[0].IsSystem -or $disk[0].PartitionStyle -ne 'RAW') {
        throw 'VHDX is not one isolated, unpartitioned virtual disk'
    }
    $number = [int]$disk[0].Number
    $disk[0] | Initialize-Disk -PartitionStyle GPT -PassThru | Out-Null
    $disk = @(Get-DiskImage -ImagePath $imagePath | Get-Disk)
    if ($disk.Count -ne 1 -or [int]$disk[0].Number -ne $number -or
        $disk[0].PartitionStyle -ne 'GPT') {
        throw 'VHDX identity or GPT initialization changed'
    }
    # An MSR partition gives Win32 a complete GPT layout but no mountable volume.
    $disk[0] | New-Partition -Size 32MB -GptType '{E3C9E316-0B5C-4DB8-817D-F92DF00215AE}' | Out-Null
    Set-Location $project
    $env:NANDUNX_W5_EMPTY_VHD_DISK = [string]$disk[0].Number
    & cargo test -p nandunx-core --locked disposable_empty_vhd_exclusive_raw_probe_when_requested -- --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'Empty VHDX exclusive RAW probe failed' }
    Write-Host 'PASS exclusive RAW handle on disposable VHDX without volumes'
} finally {
    Remove-Item -LiteralPath $diskpartPath -ErrorAction SilentlyContinue
    if ($attached) {
        Dismount-DiskImage -ImagePath $imagePath -ErrorAction Stop | Out-Null
    }
    Remove-Item -LiteralPath $imagePath -ErrorAction SilentlyContinue
}
