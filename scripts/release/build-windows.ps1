param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$')]
    [string]$Tag,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-Checked {
    param([string]$Name, [string[]]$Arguments)

    & $Name @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Name exited with code $LASTEXITCODE"
    }
}

function Read-Version {
    param([string]$Path)

    return (Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json).version
}

$version = $Tag.Substring(1)
$cargoVersion = ([regex]::Match((Get-Content -Raw -LiteralPath 'Cargo.toml'), '(?m)^version = "([^"]+)"$')).Groups[1].Value
$packageVersion = Read-Version 'package.json'
$tauriVersion = Read-Version 'src-tauri/tauri.conf.json'

if (-not $cargoVersion -or $cargoVersion -ne $packageVersion -or $cargoVersion -ne $tauriVersion) {
    throw "Version mismatch: Cargo=$cargoVersion npm=$packageVersion Tauri=$tauriVersion"
}
if ($version -ne $cargoVersion) {
    throw "Tag $Tag does not match project version $cargoVersion"
}

$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $resolvedOutput) {
    throw "Refusing to overwrite existing output directory: $resolvedOutput"
}
New-Item -ItemType Directory -Path $resolvedOutput | Out-Null

$env:Path = "$env:USERPROFILE\.cargo\bin;" +
    [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
    [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + $env:Path

Invoke-Checked 'git' @('--version')
Invoke-Checked 'node' @('--version')
Invoke-Checked 'npm.cmd' @('--version')
Invoke-Checked 'cargo' @('--version')
$rustInfo = & rustc -vV
if ($LASTEXITCODE -ne 0 -or $rustInfo -notcontains 'host: x86_64-pc-windows-msvc') {
    throw 'Rust MSVC x64 host is required'
}

Invoke-Checked 'npm.cmd' @('ci')
Invoke-Checked 'npm.cmd' @('run', 'build')
Invoke-Checked 'cargo' @('fmt', '--check')
Invoke-Checked 'cargo' @('test', '--workspace', '--locked')
Invoke-Checked 'npm.cmd' @('run', 'tauri', '--', 'build', '--config', 'src-tauri/tauri.windows.conf.json', '--bundles', 'nsis')

$installers = @(Get-ChildItem -LiteralPath 'target/release/bundle/nsis' -Filter '*.exe' -File)
if ($installers.Count -ne 1) {
    throw "Expected exactly one NSIS installer, found $($installers.Count)"
}

$asset = Join-Path $resolvedOutput "nandunx-$version-windows-x86_64-setup.exe"
Copy-Item -LiteralPath $installers[0].FullName -Destination $asset
$digest = (Get-FileHash -LiteralPath $asset -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Output "artifact=$(Split-Path -Leaf $asset)"
Write-Output "sha256=$digest"
