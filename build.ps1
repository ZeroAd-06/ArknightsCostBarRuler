# Build script for ArknightsCostBarRuler (Rust version)
# Usage: .\build.ps1 [release|debug]

param(
    [Parameter(Position = 0)]
    [ValidateSet("release", "debug")]
    [string]$Profile = "release"
)

$ErrorActionPreference = "Stop"
$ROOT = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ROOT

# Clean dist directory
if (Test-Path "dist") {
    Remove-Item -Recurse -Force "dist"
}

$buildArgs = @("build")
if ($Profile -eq "release") {
    $buildArgs += "--release"
}
$buildArgs += "-p", "ruler-app"

Write-Host "==> cargo $($buildArgs -join ' ')" -ForegroundColor Cyan
cargo @buildArgs

if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed!" -ForegroundColor Red
    exit $LASTEXITCODE
}

# Determine output directory
$targetDir = if ($Profile -eq "release") { "target\release" } else { "target\debug" }
$exePath = Join-Path $targetDir "ruler-app.exe"

if (-not (Test-Path $exePath)) {
    Write-Host "ERROR: Built binary not found at $exePath" -ForegroundColor Red
    exit 1
}

# Prepare distribution directory
$distDir = "dist\ArknightsCostBarRuler"
New-Item -ItemType Directory -Path $distDir -Force | Out-Null

# Copy binary
Copy-Item $exePath -Destination $distDir

# Copy required resources
if (Test-Path "icons") {
    Copy-Item -Recurse icons -Destination $distDir
}
if (Test-Path "ruler\locales") {
    New-Item -ItemType Directory -Path "$distDir\ruler" -Force | Out-Null
    Copy-Item -Recurse ruler\locales -Destination "$distDir\ruler"
}
if (Test-Path "LICENSE") {
    Copy-Item LICENSE -Destination $distDir
}
if (Test-Path "LICENSES") {
    Copy-Item -Recurse LICENSES -Destination $distDir
}

Write-Host "Build complete. Binary at $distDir\ruler-app.exe" -ForegroundColor Green
