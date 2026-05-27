[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$WinDivertPath,

    [Parameter(Mandatory = $true)]
    [string]$WinDivertLicensePath,

    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$manifestPath = Join-Path $projectRoot "Cargo.toml"
$manifest = Get-Content -LiteralPath $manifestPath -Raw
$versionMatch = [regex]::Match($manifest, '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) {
    throw "Could not read the package version from Cargo.toml."
}
$version = $versionMatch.Groups[1].Value

if (-not $SkipBuild) {
    Push-Location $projectRoot
    try {
        & cargo build --release
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --release failed."
        }
    }
    finally {
        Pop-Location
    }
}

$requiredFiles = @{
    "interceptor.exe"     = Join-Path $projectRoot "target\release\interceptor.exe"
    "WinDivert.dll"       = Join-Path $WinDivertPath "WinDivert.dll"
    "WinDivert64.sys"     = Join-Path $WinDivertPath "WinDivert64.sys"
    "LICENSE"             = Join-Path $projectRoot "LICENSE"
    "README.md"           = Join-Path $projectRoot "README.md"
    "WinDivert-LICENSE.txt" = $WinDivertLicensePath
}

foreach ($entry in $requiredFiles.GetEnumerator()) {
    if (-not (Test-Path -LiteralPath $entry.Value -PathType Leaf)) {
        throw "Missing required release file for $($entry.Key): $($entry.Value)"
    }
}

$distRoot = [System.IO.Path]::GetFullPath((Join-Path $projectRoot "dist"))
$packageName = "interceptor-v$version-windows-x64"
$packageDir = [System.IO.Path]::GetFullPath((Join-Path $distRoot $packageName))
$distPrefix = $distRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) +
    [System.IO.Path]::DirectorySeparatorChar
if (-not $packageDir.StartsWith($distPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to prepare a package outside the dist directory."
}

New-Item -ItemType Directory -Force -Path $distRoot | Out-Null
if (Test-Path -LiteralPath $packageDir) {
    Remove-Item -LiteralPath $packageDir -Recurse -Force
}
New-Item -ItemType Directory -Path $packageDir | Out-Null

foreach ($entry in $requiredFiles.GetEnumerator()) {
    Copy-Item -LiteralPath $entry.Value -Destination (Join-Path $packageDir $entry.Key)
}

$zipPath = Join-Path $distRoot "$packageName.zip"
if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
}
Compress-Archive -LiteralPath $packageDir -DestinationPath $zipPath -CompressionLevel Optimal

Write-Host "Release archive ready: $zipPath"
