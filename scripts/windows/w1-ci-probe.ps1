param(
    [string]$ReportPath = 'w1-ci-report.txt',
    [string]$Revision = ''
)

$ErrorActionPreference = 'Stop'
$env:Path = "$env:USERPROFILE\.cargo\bin;" +
    [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
    [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + $env:Path

function Invoke-Checked {
    param([string]$Name, [string[]]$Arguments)
    Write-Host "> $Name $($Arguments -join ' ')"
    & $Name @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Name exited with code $LASTEXITCODE"
    }
}

if (-not $Revision) {
    $Revision = & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot identify tested revision' }
}

$report = @(
    "revision=$Revision"
    "os=$([Environment]::OSVersion.VersionString)"
    "arch=$env:PROCESSOR_ARCHITECTURE"
    "date_utc=$([DateTime]::UtcNow.ToString('u'))"
)
try {
    Invoke-Checked git @('--version')
    Invoke-Checked node @('--version')
    Invoke-Checked npm.cmd @('--version')
    Invoke-Checked cargo @('--version')
    $rustInfo = & rustc -vV
    if ($LASTEXITCODE -ne 0) { throw 'rustc -vV failed' }
    $rustInfo | Write-Host
    if ($rustInfo -notcontains 'host: x86_64-pc-windows-msvc') {
        throw 'Rust host is not x86_64-pc-windows-msvc'
    }
    if ($rustInfo[0] -notmatch '^rustc 1\.90\.0 ') {
        throw 'Rust 1.90.0 is required for the W1 CI probe'
    }
    $report += "rustc=$($rustInfo[0])"
    foreach ($step in @(
        @{ Name = 'npm.cmd'; Arguments = @('ci') },
        @{ Name = 'npm.cmd'; Arguments = @('run', 'build') },
        @{ Name = 'cargo'; Arguments = @('fmt', '--check') },
        @{ Name = 'cargo'; Arguments = @('test', '--workspace', '--locked') },
        @{ Name = 'cargo'; Arguments = @('build', '-p', 'nandunx-desktop', '--locked') }
    )) {
        Invoke-Checked $step.Name $step.Arguments
        $report += "passed=$($step.Name) $($step.Arguments -join ' ')"
    }
    $report += 'result=passed'
} catch {
    $report += "result=failed"
    $report += "error=$($_.Exception.Message)"
    throw
} finally {
    $report | Set-Content -LiteralPath $ReportPath -Encoding UTF8
}
