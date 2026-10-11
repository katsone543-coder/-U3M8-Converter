#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "此脚本只能在 macOS 上运行。" >&2
  exit 1
fi

cd "$(dirname "$0")/.."
npm ci
npm run prepare:binaries
python3 scripts/generate-icon.py

icon_temp="$(mktemp -d)"
dmg_mount=""
cleanup() {
  if [[ -n "$dmg_mount" ]]; then
    hdiutil detach "$dmg_mount" >/dev/null 2>&1 || true
    rm -rf "$dmg_mount"
  fi
  rm -rf "$icon_temp"
}
trap cleanup EXIT
iconset="$icon_temp/App.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" src-tauri/icons/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
done
for size in 16 32 128 256 512; do
  pixel=$((size * 2))
  sips -z "$pixel" "$pixel" src-tauri/icons/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o src-tauri/icons/icon.icns

cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri -- build --bundles dmg --no-sign

dmg_path="$(find src-tauri/target/release/bundle/dmg -maxdepth 1 -name '*.dmg' -print -quit)"
test -n "$dmg_path"
dmg_mount="$(mktemp -d)"
hdiutil attach -quiet -readonly -nobrowse -mountpoint "$dmg_mount" "$dmg_path"
app_path="$(find "$dmg_mount" -maxdepth 1 -name '*.app' -print -quit)"
test -n "$app_path"
test -x "$app_path/Contents/MacOS/ffmpeg"
test -x "$app_path/Contents/MacOS/ffprobe"
"$app_path/Contents/MacOS/ffmpeg" -version >/dev/null
"$app_path/Contents/MacOS/ffprobe" -version >/dev/null

echo "DMG 文件："
printf '%s\n' "$dmg_path"
