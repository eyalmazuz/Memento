param(
    [switch]$DynamicLibAss
)

$ErrorActionPreference = "Stop"

# Set up native Windows MSVC dependencies for Subomi:
#   1. libmpv (prebuilt non-v3 x86_64 archive from shinchiro/mpv-winbuild-cmake + MSVC mpv.lib)
#   2. libass via vcpkg (`x64-windows-static-md` by default for minimal DLLs, or
#      `x64-windows` when -DynamicLibAss is passed as fallback)
#   3. LIBCLANG_PATH for bindgen

$depsRoot = if ($env:SUBOMI_WIN_DEPS_DIR) { $env:SUBOMI_WIN_DEPS_DIR } else { Join-Path $env:RUNNER_TEMP "subomi-win-deps" }
New-Item -ItemType Directory -Force -Path $depsRoot | Out-Null

# ── 1. libmpv SDK (shinchiro non-v3 x86_64) ──────────────────────────────────
$mpvDir = Join-Path $depsRoot "mpv-sdk"
if (-not (Test-Path (Join-Path $mpvDir "mpv.lib")) -or -not (Test-Path (Join-Path $mpvDir "libmpv-2.dll"))) {
    New-Item -ItemType Directory -Force -Path $mpvDir | Out-Null

    $headers = @{ "User-Agent" = "Subomi-CI" }
    if ($env:GH_TOKEN) {
        $headers["Authorization"] = "Bearer $($env:GH_TOKEN)"
    }
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest" -Headers $headers
    # Match baseline x86_64 (exclude `-v3-` AVX2-only builds so older CPUs work too).
    $asset = $release.assets | Where-Object {
        $_.name -match '^mpv-dev-x86_64-[0-9].*\.7z$'
    } | Select-Object -First 1
    if (-not $asset) {
        throw "Could not find baseline mpv-dev-x86_64-*.7z asset in shinchiro/mpv-winbuild-cmake latest release"
    }

    Write-Host "Downloading $($asset.name)..."
    $archivePath = Join-Path $depsRoot $asset.name
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $archivePath -Headers $headers

    & 7z x $archivePath "-o$mpvDir" -y | Out-Null
    Remove-Item -Force $archivePath

    $defFile = Get-ChildItem -Path $mpvDir -Filter "mpv.def" -Recurse | Select-Object -First 1
    if (-not $defFile) {
        throw "mpv.def not found after extracting $($asset.name)"
    }

    # Locate MSVC lib.exe via vswhere
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    $vsInstall = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vsInstall) {
        throw "Could not locate Visual Studio MSVC tools via vswhere"
    }
    $libExe = Get-ChildItem -Path (Join-Path $vsInstall "VC\Tools\MSVC") -Filter "lib.exe" -Recurse |
        Where-Object { $_.FullName -match 'Hostx64\\x64\\lib\.exe$' } |
        Select-Object -First 1
    if (-not $libExe) {
        throw "Could not locate x64 MSVC lib.exe under $vsInstall"
    }

    & $libExe.FullName "/def:$($defFile.FullName)" "/out:$(Join-Path $mpvDir 'mpv.lib')" "/machine:x64"
    if ($LASTEXITCODE -ne 0) {
        throw "lib.exe failed to generate mpv.lib"
    }
}

$mpvInclude = if (Test-Path (Join-Path $mpvDir "include\mpv\client.h")) {
    Join-Path $mpvDir "include"
} elseif (Test-Path (Join-Path $mpvDir "mpv\client.h")) {
    $mpvDir
} else {
    throw "Could not locate mpv/client.h inside $mpvDir"
}

$mpvDll = Get-ChildItem -Path $mpvDir -Filter "libmpv-2.dll" -Recurse | Select-Object -First 1
if (-not $mpvDll) {
    throw "libmpv-2.dll not found inside $mpvDir"
}

# ── 2. libass via vcpkg ──────────────────────────────────────────────────────
$vcpkgRoot = if ($env:VCPKG_INSTALLATION_ROOT) { $env:VCPKG_INSTALLATION_ROOT } else { "C:\vcpkg" }
$vcpkgExe = Join-Path $vcpkgRoot "vcpkg.exe"
if (-not (Test-Path $vcpkgExe)) {
    throw "vcpkg.exe not found at $vcpkgExe"
}

$triplet = if ($DynamicLibAss) { "x64-windows" } else { "x64-windows-static-md" }
Write-Host "Installing libass:${triplet} via vcpkg..."
& $vcpkgExe install "libass:${triplet}"
if ($LASTEXITCODE -ne 0) {
    throw "vcpkg install libass:${triplet} failed"
}

$vcpkgInstalled = Join-Path $vcpkgRoot "installed\${triplet}"
$assLibDir = Join-Path $vcpkgInstalled "lib"
if (-not (Test-Path (Join-Path $assLibDir "ass.lib"))) {
    throw "ass.lib not found at $assLibDir"
}

$assExtraLibs = ""
$assDllDir = ""
if ($DynamicLibAss) {
    $assDllDir = Join-Path $vcpkgInstalled "bin"
} else {
    # Link all transitive static .lib dependencies installed by vcpkg alongside ass.lib,
    # plus standard Win32 libraries required by DirectWrite/HarfBuzz/FreeType.
    $staticLibs = Get-ChildItem -Path $assLibDir -Filter "*.lib" |
        Where-Object { $_.BaseName -ne "ass" } |
        ForEach-Object { $_.BaseName }
    $winSysLibs = @("dwrite", "gdi32", "usp10", "rpcrt4", "user32", "advapi32", "ole32")
    $assExtraLibs = (($staticLibs + $winSysLibs) | Select-Object -Unique) -join ","
}

# ── 3. Locate LLVM libclang for bindgen ──────────────────────────────────────
$libclangPath = $env:LIBCLANG_PATH
if (-not $libclangPath) {
    foreach ($candidate in @("C:\Program Files\LLVM\bin", "C:\Program Files (x86)\LLVM\bin")) {
        if (Test-Path (Join-Path $candidate "libclang.dll")) {
            $libclangPath = $candidate
            break
        }
    }
}

# ── 4. Export environment variables to GitHub Actions (and current process) ──
$exports = @{
    "MPV_SDK"          = $mpvDir
    "MPV_INCLUDE_DIR"  = $mpvInclude
    "MPV_LIB_DIR"      = $mpvDir
    "MPV_DLL_PATH"     = $mpvDll.FullName
    "ASS_LIB_DIR"      = $assLibDir
    "ASS_EXTRA_LIBS"   = $assExtraLibs
    "ASS_DLL_DIR"      = $assDllDir
    "ASS_TRIPLET"      = $triplet
}
if ($libclangPath) {
    $exports["LIBCLANG_PATH"] = $libclangPath
}

foreach ($kv in $exports.GetEnumerator()) {
    Set-Item -Path "Env:$($kv.Key)" -Value $kv.Value
    if ($env:GITHUB_ENV) {
        "$($kv.Key)=$($kv.Value)" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
    }
}

$pathAdditions = @($mpvDll.DirectoryName)
if ($assDllDir -and (Test-Path $assDllDir)) {
    $pathAdditions += $assDllDir
}
foreach ($dir in $pathAdditions) {
    $env:PATH = "${dir};$($env:PATH)"
    if ($env:GITHUB_PATH) {
        $dir | Out-File -FilePath $env:GITHUB_PATH -Encoding utf8 -Append
    }
}

Write-Host "Windows deps ready: mpv=$($mpvDll.FullName), libass=${triplet} (extra libs: ${assExtraLibs})"
