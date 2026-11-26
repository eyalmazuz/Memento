#!/usr/bin/env bash
set -euo pipefail

# Package a compiled Linux `subomi` binary into BOTH:
#   1. A lightweight ELF tarball:   subomi-linux-<arch><suffix>.tar.gz
#   2. A self-contained AppImage:   Subomi-linux-<arch><suffix>.AppImage
#
# Required env vars:
#   BINARY_PATH     Path to the compiled `subomi` ELF binary
#   ARCH            Target arch label: `x86_64` or `arm64`
# Optional env vars:
#   VARIANT_SUFFIX  e.g. `` (base) or `-ocr-asr-vulkan`
#   OUT_DIR         Output directory (default: `dist`)

: "${BINARY_PATH:?BINARY_PATH must be set}"
: "${ARCH:?ARCH must be set (x86_64 or arm64)}"
VARIANT_SUFFIX="${VARIANT_SUFFIX:-}"
OUT_DIR="${OUT_DIR:-dist}"

if [[ ! -f "$BINARY_PATH" ]]; then
  echo "::error::Binary not found at ${BINARY_PATH}"
  exit 1
fi

case "$ARCH" in
  x86_64) APPIMAGE_ARCH="x86_64" ;;
  arm64|aarch64) APPIMAGE_ARCH="aarch64" ;;
  *)
    echo "::error::Unsupported ARCH=${ARCH}"
    exit 1
    ;;
esac

mkdir -p "$OUT_DIR"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# ── Smoke-test the raw binary ────────────────────────────────────────────────
chmod +x "$BINARY_PATH"
"$BINARY_PATH" --version

# ── 1. Raw ELF tarball (.tar.gz) ─────────────────────────────────────────────
TAR_STEM="subomi-linux-${ARCH}${VARIANT_SUFFIX}"
TAR_STAGE="${WORK_DIR}/${TAR_STEM}"
mkdir -p "$TAR_STAGE"
cp "$BINARY_PATH" "${TAR_STAGE}/subomi"
chmod +x "${TAR_STAGE}/subomi"
cp res/subomi.desktop "${TAR_STAGE}/subomi.desktop"
cp res/subomi.svg "${TAR_STAGE}/subomi.svg"
cp res/subomi.png "${TAR_STAGE}/subomi.png"
cp LICENSE "${TAR_STAGE}/LICENSE"

tar -C "$WORK_DIR" -czf "${OUT_DIR}/${TAR_STEM}.tar.gz" "$TAR_STEM"
test -s "${OUT_DIR}/${TAR_STEM}.tar.gz"
echo "Created ${OUT_DIR}/${TAR_STEM}.tar.gz"

# ── 2. Self-contained AppImage ───────────────────────────────────────────────
APPDIR="${WORK_DIR}/Subomi.AppDir"
mkdir -p "${APPDIR}/usr/bin" "${APPDIR}/usr/lib" "${APPDIR}/usr/share/applications" "${APPDIR}/usr/share/icons/hicolor/512x512/apps"

cp "$BINARY_PATH" "${APPDIR}/usr/bin/subomi"
chmod +x "${APPDIR}/usr/bin/subomi"

# Strip leading shebang line (`#!/usr/bin/env xdg-open`) and normalize desktop file
# so desktop-file-validate inside appimagetool passes cleanly.
grep -v '^#!' res/subomi.desktop \
  | sed 's|video/x-ms-wvxvideo;|video/x-ms-wvx;|g' \
  > "${APPDIR}/subomi.desktop"
cp "${APPDIR}/subomi.desktop" "${APPDIR}/usr/share/applications/subomi.desktop"
cp res/subomi.png "${APPDIR}/subomi.png"
cp res/subomi.png "${APPDIR}/.DirIcon"
cp res/subomi.png "${APPDIR}/usr/share/icons/hicolor/512x512/apps/subomi.png"

cat > "${APPDIR}/AppRun" << 'EOF'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="${HERE}/usr/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
export PATH="${HERE}/usr/bin:${PATH}"
exec "${HERE}/usr/bin/subomi" "$@"
EOF
chmod +x "${APPDIR}/AppRun"

# Download the official AppImage excludelist (with a built-in fallback) so we
# bundle libmpv/libass/FFmpeg while leaving host glibc, GPU/GL/Vulkan, Wayland/X11,
# and audio daemons to the host OS.
EXCLUDELIST="${WORK_DIR}/excludelist"
curl -fsSL "https://raw.githubusercontent.com/AppImageCommunity/pkg2appimage/master/excludelist" \
  -o "$EXCLUDELIST" 2>/dev/null || touch "$EXCLUDELIST"

# Always enforce critical host driver / core library exclusions on top of upstream list.
cat >> "$EXCLUDELIST" << 'EOF'
ld-linux-x86-64.so.2
ld-linux-aarch64.so.1
libc.so.6
libm.so.6
libdl.so.2
librt.so.1
libpthread.so.0
libresolv.so.2
libgcc_s.so.1
libstdc++.so.6
libGL.so.1
libGLX.so.0
libGLdispatch.so.0
libEGL.so.1
libGLESv2.so.2
libOpenGL.so.0
libvulkan.so.1
libdrm.so.2
libgbm.so.1
libva.so.2
libva-drm.so.2
libva-x11.so.2
libva-wayland.so.2
libvdpau.so.1
libwayland-client.so.0
libwayland-cursor.so.0
libwayland-egl.so.1
libwayland-server.so.0
libX11.so.6
libX11-xcb.so.1
libxcb.so.1
libxkbcommon.so.0
libasound.so.2
libpulse.so.0
libpipewire-0.3.so.0
libjack.so.0
libdbus-1.so.3
libfontconfig.so.1
libfreetype.so.6
libexpat.so.1
libz.so.1
libuuid.so.1
libsystemd.so.0
libudev.so.1
EOF

is_excluded() {
  local base="$1"
  # Fast pattern exclusions for host driver/NSS/X11/Wayland families
  case "$base" in
    ld-linux*.so*|libc.so*|libm.so*|libdl.so*|librt.so*|libpthread.so*|libresolv.so*|\
    libnss_*.so*|libnsl.so*|libutil.so*|libcrypt.so*|libgcc_s.so*|libstdc++.so*|\
    libGL*.so*|libEGL*.so*|libGLES*.so*|libOpenGL.so*|libvulkan.so*|\
    libdrm*.so*|libgbm.so*|libva*.so*|libvdpau.so*|\
    libwayland-*.so*|libX11*.so*|libxcb*.so*|libXext.so*|libXrender.so*|libXrandr.so*|\
    libXfixes.so*|libXcursor.so*|libXi.so*|libXinerama.so*|libXss.so*|libXxf86vm.so*|\
    libXdamage.so*|libXcomposite.so*|libxkbcommon*.so*|libICE.so*|libSM.so*|\
    libasound.so*|libpulse*.so*|libpipewire*.so*|libjack*.so*|libdbus-1.so*|\
    libfontconfig.so*|libfreetype.so*|libexpat.so*|libz.so*|libuuid.so*|\
    libsystemd.so*|libudev.so*|libcap.so*|libselinux.so*|libgpg-error.so*|\
    libglib-2.0.so*|libgobject-2.0.so*|libgio-2.0.so*|libgmodule-2.0.so*|libgthread-2.0.so*)
      return 0
      ;;
  esac
  # Check exact SONAME entries in upstream excludelist (ignoring comments)
  awk '{ sub(/#.*/, ""); gsub(/^[ \t]+|[ \t]+$/, ""); if (length($0) > 0) print $0 }' "$EXCLUDELIST" \
    | grep -Fxq "$base"
}

# Iteratively resolve and copy all non-excluded shared libraries into AppDir/usr/lib.
declare -A SEEN_LIBS=()
queue=("${APPDIR}/usr/bin/subomi")

while [[ ${#queue[@]} -gt 0 ]]; do
  current="${queue[0]}"
  queue=("${queue[@]:1}")

  while IFS= read -r lib_path; do
    [[ -z "$lib_path" || ! -f "$lib_path" ]] && continue
    soname="$(basename "$lib_path")"
    if [[ -n "${SEEN_LIBS[$soname]+x}" ]]; then
      continue
    fi
    SEEN_LIBS["$soname"]=1
    if is_excluded "$soname"; then
      continue
    fi
    dest="${APPDIR}/usr/lib/${soname}"
    if [[ ! -f "$dest" ]]; then
      cp -L "$lib_path" "$dest"
      chmod 0644 "$dest"
      queue+=("$dest")
    fi
  done < <(ldd "$current" 2>/dev/null | awk '/=> \// { print $3 }' || true)
done

bundled_count="$(find "${APPDIR}/usr/lib" -maxdepth 1 -type f | wc -l)"
echo "Bundled ${bundled_count} shared libraries into AppDir/usr/lib"

# Download appimagetool for the matching architecture and build the AppImage.
APPIMAGETOOL="${WORK_DIR}/appimagetool.AppImage"
curl -fsSL \
  "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${APPIMAGE_ARCH}.AppImage" \
  -o "$APPIMAGETOOL"
chmod +x "$APPIMAGETOOL"

APPIMAGE_OUT="${OUT_DIR}/Subomi-linux-${ARCH}${VARIANT_SUFFIX}.AppImage"
ARCH="$APPIMAGE_ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGETOOL" --no-appstream "$APPDIR" "$APPIMAGE_OUT"
chmod +x "$APPIMAGE_OUT"
test -s "$APPIMAGE_OUT"

# Smoke-test the produced AppImage without needing FUSE on the runner.
APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGE_OUT" --version
echo "Created and verified ${APPIMAGE_OUT}"
