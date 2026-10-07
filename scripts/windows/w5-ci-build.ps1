param([string]$ReportPath = 'w5-ci-report.txt')

$ErrorActionPreference = 'Stop'
$env:Path = "$env:USERPROFILE\.cargo\bin;" +
    [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
    [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + $env:Path

function Invoke-Checked {
    param([string]$Name, [string[]]$Arguments)
    & $Name @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Name exited with code $LASTEXITCODE"
    }
}

$revision = & git rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify tested revision' }
$report = @("revision=$revision", "date_utc=$([DateTime]::UtcNow.ToString('u'))")
try {
    $buildDrive = [System.IO.DriveInfo]::new([System.IO.Path]::GetPathRoot((Get-Location).Path))
    $freeGiB = [math]::Round($buildDrive.AvailableFreeSpace / 1GB, 2)
    $report += "free_gib=$freeGiB"
    if ($buildDrive.AvailableFreeSpace -lt 8GB) {
        throw "Windows build requires at least 8 GiB free; available: $freeGiB GiB. Remove old temporary build outputs before retrying."
    }
    Invoke-Checked git @('--version')
    Invoke-Checked node @('--version')
    Invoke-Checked npm.cmd @('--version')
    Invoke-Checked cargo @('--version')
    $rustInfo = & rustc -vV
    if ($LASTEXITCODE -ne 0 -or $rustInfo -notcontains 'host: x86_64-pc-windows-msvc') {
        throw 'Rust MSVC x64 host is required'
    }
    $rustInfo | Write-Host
    $version = (Get-Content -Raw -LiteralPath 'package.json' | ConvertFrom-Json).version
    $tauriVersion = (Get-Content -Raw -LiteralPath 'src-tauri/tauri.conf.json' | ConvertFrom-Json).version
    $metadata = & cargo metadata --no-deps --locked --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read workspace versions' }
    $cargoVersions = @($metadata.packages |
        Where-Object { $_.name -in @('nandunx-core', 'nandunx-desktop', 'nandunx-web') } |
        ForEach-Object { $_.version } | Select-Object -Unique)
    if (-not $version -or $version -ne $tauriVersion -or
        $cargoVersions.Count -ne 1 -or $version -ne $cargoVersions[0]) {
        throw "Version mismatch: npm=$version Tauri=$tauriVersion Cargo=$($cargoVersions -join ',')"
    }
    foreach ($step in @(
        @{ Name = 'npm.cmd'; Arguments = @('ci') },
        @{ Name = 'npm.cmd'; Arguments = @('run', 'build') },
        @{ Name = 'cargo'; Arguments = @('fmt', '--check') },
        @{ Name = 'cargo'; Arguments = @('test', '--workspace', '--locked') },
        @{ Name = 'cargo'; Arguments = @('test', '-p', 'nandunx-desktop', '--features', 'custom-protocol', '--locked') },
        @{ Name = 'cargo'; Arguments = @('build', '-p', 'nandunx-desktop', '--features', 'custom-protocol', '--locked') }
    )) {
        Invoke-Checked $step.Name $step.Arguments
        $report += "passed=$($step.Name) $($step.Arguments -join ' ')"
    }
    $output = Join-Path (Get-Location) 'windows-test'
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    $shortRevision = $revision.Substring(0, 7)
    $archive = Join-Path $output "nandunx-$version-$shortRevision-windows-x86_64-test.zip"
    $desktop = Join-Path (Get-Location) 'target/debug/nandunx-desktop.exe'
    if (-not (Test-Path -LiteralPath $desktop)) {
        throw 'Windows desktop executable is required'
    }
    Compress-Archive -LiteralPath $desktop -DestinationPath $archive -Force
    $digest = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$digest *$(Split-Path -Leaf $archive)" | Set-Content -LiteralPath (Join-Path $output 'SHA256SUMS') -Encoding ascii
    $report += "artifact=$(Split-Path -Leaf $archive)"
    $report += "sha256=$digest"
    $report += 'result=passed'
} catch {
    $report += 'result=failed'
    $report += "error=$($_.Exception.Message)"
    throw
} finally {
    $report | Set-Content -LiteralPath $ReportPath -Encoding UTF8
}
