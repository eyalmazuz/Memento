#!/usr/bin/env bash
set -euo pipefail

# Package a compiled macOS `subomi` binary into a standalone, ad-hoc signed
# `Subomi.app` bundle archived as `Subomi-macos-<arch>.zip`.
#
# Required env vars:
#   BINARY_PATH   Path to the compiled `subomi` Mach-O binary
#   ARCH          `arm64` or `x86_64`
# Optional env vars:
#   VERSION       Bundle short version string (default: read from Cargo.toml)
#   OUT_DIR       Output directory (default: `dist`)

: "${BINARY_PATH:?BINARY_PATH must be set}"
: "${ARCH:?ARCH must be set (arm64 or x86_64)}"
OUT_DIR="${OUT_DIR:-dist}"

if [[ ! -f "$BINARY_PATH" ]]; then
  echo "::error::Binary not found at ${BINARY_PATH}"
  exit 1
fi

if [[ -z "${VERSION:-}" ]]; then
  VERSION="$(awk -F'"' '/^\[workspace\.package\]/{in_ws=1;next} in_ws && /^version[[:space:]]*=/{print $2; exit}' Cargo.toml)"
fi
VERSION="${VERSION:-0.0.0}"

mkdir -p "$OUT_DIR"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

APP_BUNDLE="${WORK_DIR}/Subomi.app"
mkdir -p "${APP_BUNDLE}/Contents/MacOS" "${APP_BUNDLE}/Contents/Resources" "${APP_BUNDLE}/Contents/Frameworks"

cp "$BINARY_PATH" "${APP_BUNDLE}/Contents/MacOS/subomi"
chmod +x "${APP_BUNDLE}/Contents/MacOS/subomi"
cp res/subomi.icns "${APP_BUNDLE}/Contents/Resources/subomi.icns"
cp LICENSE "${APP_BUNDLE}/Contents/Resources/LICENSE"

cat > "${APP_BUNDLE}/Contents/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>Subomi</string>
  <key>CFBundleExecutable</key>
  <string>subomi</string>
  <key>CFBundleIconFile</key>
  <string>subomi.icns</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.eyalmazuz.subomi</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Subomi</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${VERSION}</string>
  <key>CFBundleVersion</key>
  <string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key>
  <true/>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key>
      <string>Video &amp; Audio Media</string>
      <key>CFBundleTypeRole</key>
      <string>Viewer</string>
      <key>LSHandlerRank</key>
      <string>Alternate</string>
      <key>CFBundleTypeExtensions</key>
      <array>
        <string>mkv</string>
        <string>mp4</string>
        <string>m4v</string>
        <string>webm</string>
        <string>mov</string>
        <string>avi</string>
        <string>ts</string>
        <string>m2ts</string>
        <string>flv</string>
        <string>ogv</string>
        <string>mp3</string>
        <string>m4a</string>
        <string>flac</string>
        <string>ogg</string>
        <string>opus</string>
        <string>wav</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
EOF

# Bundle Homebrew / non-system dylibs (libmpv, libass, FFmpeg, etc.) into
# Contents/Frameworks and rewrite all install names to @executable_path/../Frameworks/.
HOMEBREW_PREFIX="$(brew --prefix)"
dylibbundler -od -b \
  -x "${APP_BUNDLE}/Contents/MacOS/subomi" \
  -d "${APP_BUNDLE}/Contents/Frameworks" \
  -p '@executable_path/../Frameworks/' \
  -s "${HOMEBREW_PREFIX}/lib"

# Verify no Homebrew paths remain in the executable or bundled Frameworks.
echo "Checking Mach-O library links..."
leaked=0
for bin in "${APP_BUNDLE}/Contents/MacOS/subomi" "${APP_BUNDLE}/Contents/Frameworks/"*.dylib; do
  [[ -f "$bin" ]] || continue
  if otool -L "$bin" | tail -n +2 | grep -E '(/opt/homebrew/|/usr/local/Cellar/|/usr/local/opt/)' >/dev/null; then
    echo "::error::Leaked non-bundled dylib reference in ${bin}:"
    otool -L "$bin"
    leaked=1
  fi
done
if [[ "$leaked" -ne 0 ]]; then
  exit 1
fi

# macOS 15 dyld aborts on duplicate LC_RPATH entries (which dylibbundler can emit).
# Deduplicate LC_RPATHs and drop any residual Homebrew rpaths before signing.
for bin in "${APP_BUNDLE}/Contents/MacOS/subomi" "${APP_BUNDLE}/Contents/Frameworks/"*.dylib; do
  [[ -f "$bin" ]] || continue
  chmod u+w "$bin"
  rpaths="$(otool -l "$bin" | awk '/cmd LC_RPATH/{f=1;next} f && /path /{print $2; f=0}')"
  if [[ -n "$rpaths" ]]; then
    while IFS= read -r rp; do
      [[ -z "$rp" ]] && continue
      while install_name_tool -delete_rpath "$rp" "$bin" 2>/dev/null; do :; done
    done < <(printf '%s\n' "$rpaths" | sort -u)
    while IFS= read -r rp; do
      [[ -z "$rp" ]] && continue
      case "$rp" in
        /opt/homebrew/*|/usr/local/*) continue ;;
      esac
      install_name_tool -add_rpath "$rp" "$bin"
    done < <(printf '%s\n' "$rpaths" | sed -E 's|/+$||' | sort -u)
  fi
done

# Ad-hoc code sign all dylibs first, then the app bundle (required on Apple Silicon).
find "${APP_BUNDLE}/Contents/Frameworks" -type f -name '*.dylib' -exec \
  codesign --force --sign - {} +
codesign --force --deep --sign - "$APP_BUNDLE"
codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"

# Smoke-test the bundled binary directly.
"${APP_BUNDLE}/Contents/MacOS/subomi" --version

ZIP_OUT="${OUT_DIR}/Subomi-macos-${ARCH}.zip"
ditto -c -k --norsrc --noextattr --keepParent "$APP_BUNDLE" "$ZIP_OUT"
test -s "$ZIP_OUT"
echo "Created and verified ${ZIP_OUT}"
