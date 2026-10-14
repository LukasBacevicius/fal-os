#!/bin/sh
# Builds fal-os.app and fal-os-<version>-arm64.dmg for sharing with colleagues (docs/PACKAGING.md).
#
#   scripts/package-macos.sh                  build everything, assemble, sign, make the DMG
#   SKIP_BUILD=1 scripts/package-macos.sh     reuse existing build outputs (assemble/sign/DMG only)
#
# Env:
#   OUT_DIR         where fal-os.app and the DMG go (default: $CARGO_TARGET_DIR/package or target/package)
#   NODE_DIST       a Node >= 22 distribution to bundle (dir with bin/node and lib/node_modules/npm);
#                   default: the one `node` on PATH comes from (an official nodejs.org / nvm build —
#                   Homebrew's node links Homebrew dylibs and won't work on other Macs)
#   SIGN_IDENTITY   codesign identity (default "-": ad-hoc). A "Developer ID Application: …" identity
#                   turns on the hardened runtime + entitlements; then run notarize (docs/PACKAGING.md).
#   MIN_FREE_GB     refuse to start with less free disk than this (default 4)
#
# What ends up in the bundle (Tauri's externalBin layout, assembled by hand so no tauri-cli is needed):
#   Contents/MacOS/falos-desktop      the Tauri shell (frontend embedded via custom-protocol)
#   Contents/MacOS/falos-host         sidecar: MCP + sync server; spawns the agent runtime
#   Contents/Resources/node/          Node runtime (bin/node, npm/npx) for the runtime and the agents
#   Contents/Resources/runtime/       agent runtime bundled by esbuild (dist/main.mjs, dist/setup.mjs),
#                                     template sources and scripts; templates, chrome-headless-shell
#                                     and skills are installed on first run into ~/.fal-os
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT_DIR="${OUT_DIR:-$CARGO_TARGET_DIR/package}"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
MIN_FREE_GB="${MIN_FREE_GB:-4}"
APP_NAME="fal OS"
FILE_NAME=fal-os  # DMG file name: no spaces, easy to share
VERSION=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' apps/desktop/src-tauri/tauri.conf.json)
BUILD=$(git rev-parse --short HEAD 2>/dev/null || echo dev)
ARCH=$(uname -m)

say() { printf '\n==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

free_gb() { df -g "$1" | awk 'NR==2 {print $4}'; }
check_disk() {
  f=$(free_gb "$CARGO_TARGET_DIR" 2>/dev/null || free_gb "$ROOT")
  [ "$f" -ge "$MIN_FREE_GB" ] || die "only ${f} GB free (need ${MIN_FREE_GB}); free some disk or set MIN_FREE_GB"
}

[ "$(uname -s)" = Darwin ] || die "macOS only"
[ "$ARCH" = arm64 ] || echo "warning: building on $ARCH; the DMG will be $ARCH-only"
mkdir -p "$CARGO_TARGET_DIR" "$OUT_DIR"
check_disk

# ---- Node to bundle ----------------------------------------------------------------------------
if [ -z "${NODE_DIST:-}" ]; then
  NODE_BIN=$(command -v node) || die "node not found (set NODE_DIST)"
  NODE_DIST=$(cd "$(dirname "$(realpath "$NODE_BIN")")/.." && pwd)
fi
[ -x "$NODE_DIST/bin/node" ] && [ -d "$NODE_DIST/lib/node_modules/npm" ] || die "NODE_DIST=$NODE_DIST has no bin/node + lib/node_modules/npm"
NODE_MAJOR=$("$NODE_DIST/bin/node" -p 'process.versions.node.split(".")[0]')
[ "$NODE_MAJOR" -ge 22 ] || die "need Node >= 22 (NODE_DIST has $NODE_MAJOR)"
if otool -L "$NODE_DIST/bin/node" | grep -qE '/opt/homebrew|/usr/local/(opt|Cellar)'; then
  die "$NODE_DIST/bin/node links Homebrew libraries; use an official build (nvm, or nodejs.org tarball) via NODE_DIST"
fi

# ---- build -------------------------------------------------------------------------------------
if [ -z "${SKIP_BUILD:-}" ]; then
  say "web: wasm + vite build"
  pnpm --dir apps/web install --frozen-lockfile
  pnpm --dir apps/web build
  check_disk

  say "agent runtime: esbuild bundle"
  pnpm --dir apps/agent-runtime install --frozen-lockfile
  pnpm --dir apps/agent-runtime bundle

  say "falos-host (release)"
  cargo build --release -p falos-host
  check_disk

  say "desktop shell (release, embedded frontend)"
  cargo build --release --features custom-protocol --manifest-path apps/desktop/src-tauri/Cargo.toml
fi
for f in "$CARGO_TARGET_DIR/release/falos-desktop" "$CARGO_TARGET_DIR/release/falos-host" \
  apps/agent-runtime/dist/main.mjs apps/agent-runtime/dist/setup.mjs apps/web/dist/index.html; do
  [ -e "$f" ] || die "missing build output $f (run without SKIP_BUILD)"
done
# A shared CARGO_TARGET_DIR (several checkouts/worktrees) may hold another checkout's binary:
# cargo's dep-info lists the sources it was built from; refuse anything not built from $ROOT.
for b in falos-host:crates/host/src/main.rs falos-desktop:apps/desktop/src-tauri/src/main.rs; do
  bin=${b%%:*} src=${b#*:}
  grep -qF "$ROOT/$src" "$CARGO_TARGET_DIR/release/$bin.d" 2>/dev/null ||
    die "$CARGO_TARGET_DIR/release/$bin was built from another checkout (rebuild without SKIP_BUILD)"
done

# ---- assemble ----------------------------------------------------------------------------------
say "assemble $APP_NAME.app"
APP="$OUT_DIR/$APP_NAME.app"
rm -rf "$APP"
C="$APP/Contents"
mkdir -p "$C/MacOS" "$C/Resources"

cp "$CARGO_TARGET_DIR/release/falos-desktop" "$C/MacOS/falos-desktop"
cp "$CARGO_TARGET_DIR/release/falos-host" "$C/MacOS/falos-host"
strip -S "$C/MacOS/falos-desktop" "$C/MacOS/falos-host" # debug info out (release profile keeps it)

# Node: the binary (keeps its own OpenJS Developer ID signature) + npm/npx.
N="$C/Resources/node"
mkdir -p "$N/bin" "$N/lib/node_modules"
cp "$NODE_DIST/bin/node" "$N/bin/node"
cp -R "$NODE_DIST/lib/node_modules/npm" "$N/lib/node_modules/npm"
ln -s ../lib/node_modules/npm/bin/npm-cli.js "$N/bin/npm"
ln -s ../lib/node_modules/npm/bin/npx-cli.js "$N/bin/npx"
[ -f "$NODE_DIST/LICENSE" ] && cp "$NODE_DIST/LICENSE" "$N/LICENSE"

# Agent runtime: bundle + what it reads relative to itself.
R="$C/Resources/runtime"
mkdir -p "$R/dist" "$R/scripts" "$R/motion-template" "$R/hyperframes-template"
cp apps/agent-runtime/dist/main.mjs apps/agent-runtime/dist/setup.mjs "$R/dist/"
for f in apps/agent-runtime/dist/*.LEGAL.txt; do [ -e "$f" ] && cp "$f" "$R/dist/"; done
cp apps/agent-runtime/package.json "$R/package.json" # host's runtime detection looks for it
cp apps/agent-runtime/scripts/motion-qa.mjs apps/agent-runtime/scripts/install-skills.sh "$R/scripts/"
cp -R apps/agent-runtime/motion-template/src apps/agent-runtime/motion-template/package.json \
  apps/agent-runtime/motion-template/tsconfig.json apps/agent-runtime/motion-template/remotion.config.ts "$R/motion-template/"
cp apps/agent-runtime/hyperframes-template/index.html apps/agent-runtime/hyperframes-template/package.json "$R/hyperframes-template/"

# Icon: the LED fal mark, all sizes 16-1024 (assets/brand/tools/generate.py renders it).
cp apps/desktop/src-tauri/icons/icon.icns "$C/Resources/icon.icns"

cat > "$C/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleExecutable</key><string>falos-desktop</string>
  <key>CFBundleIdentifier</key><string>ai.fal.os</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION+$BUILD</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.graphics-design</string>
  <key>LSMinimumSystemVersion</key><string>26.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>fal internal build — not for distribution outside fal</string>
</dict>
</plist>
PLIST

# ---- sign --------------------------------------------------------------------------------------
say "codesign ($SIGN_IDENTITY)"
if [ "$SIGN_IDENTITY" = "-" ]; then
  # Ad-hoc: inner executables first, then the bundle. Node keeps its OpenJS signature.
  codesign --force --sign - "$C/MacOS/falos-host"
  codesign --force --sign - --identifier ai.fal.os "$APP"
else
  # Developer ID: hardened runtime + timestamp everywhere (notarization requires both).
  ENT="$ROOT/apps/desktop/src-tauri/entitlements.plist"
  NODE_ENT="$ROOT/apps/desktop/src-tauri/node-entitlements.plist"
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" --entitlements "$NODE_ENT" "$N/bin/node"
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$C/MacOS/falos-host"
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" --entitlements "$ENT" "$APP"
fi
codesign --verify --deep --strict "$APP"
codesign -dv "$APP" 2>&1 | grep -E '^(Identifier|Signature|TeamIdentifier|Format)' || true

# ---- DMG ---------------------------------------------------------------------------------------
say "DMG"
DMG="$OUT_DIR/$FILE_NAME-$VERSION-$ARCH.dmg"
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGE" -fs APFS -format ULFO -ov "$DMG"
rm -rf "$STAGE"
[ "$SIGN_IDENTITY" = "-" ] || codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"

say "done"
du -sh "$APP" "$DMG"
echo "app: $APP"
echo "dmg: $DMG"
