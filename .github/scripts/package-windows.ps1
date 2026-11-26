param(
    [Parameter(Mandatory = $true)]
    [string]$BinaryPath,
    [string]$OutDir = "dist"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $BinaryPath)) {
    throw "Binary not found at $BinaryPath"
}
if (-not $env:MPV_DLL_PATH -or -not (Test-Path $env:MPV_DLL_PATH)) {
    throw "MPV_DLL_PATH is not set or missing"
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$stageRoot = Join-Path $env:RUNNER_TEMP "subomi-win-stage"
if (Test-Path $stageRoot) {
    Remove-Item -Recurse -Force $stageRoot
}
$stageDir = Join-Path $stageRoot "Subomi-windows-x86_64"
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

Copy-Item -Path $BinaryPath -Destination (Join-Path $stageDir "subomi.exe")
Copy-Item -Path $env:MPV_DLL_PATH -Destination (Join-Path $stageDir "libmpv-2.dll")
Copy-Item -Path "LICENSE" -Destination (Join-Path $stageDir "LICENSE")

# If dynamic libass fallback was used, copy its runtime DLLs alongside subomi.exe.
if ($env:ASS_DLL_DIR -and (Test-Path $env:ASS_DLL_DIR)) {
    Get-ChildItem -Path $env:ASS_DLL_DIR -Filter "*.dll" | ForEach-Object {
        Copy-Item -Path $_.FullName -Destination (Join-Path $stageDir $_.Name)
    }
}

Write-Host "Staged Windows portable directory contents:"
Get-ChildItem -Path $stageDir | Format-Table Name, Length -AutoSize

# Smoke-test launching the packaged binary directly (without inheriting build PATH).
$savedPath = $env:PATH
try {
    $env:PATH = "$stageDir;C:\Windows\System32;C:\Windows"
    & (Join-Path $stageDir "subomi.exe") --version
    if ($LASTEXITCODE -ne 0) {
        throw "Packaged subomi.exe --version exited with code $LASTEXITCODE"
    }
} finally {
    $env:PATH = $savedPath
}

$zipOut = Join-Path $OutDir "Subomi-windows-x86_64.zip"
if (Test-Path $zipOut) {
    Remove-Item -Force $zipOut
}
Compress-Archive -Path $stageDir -DestinationPath $zipOut -CompressionLevel Optimal
if (-not (Test-Path $zipOut)) {
    throw "Failed to create $zipOut"
}
Write-Host "Created and verified $zipOut"
