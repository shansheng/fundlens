#!/usr/bin/env bash
set -e
CACHE=/home/sheng/.cache/tauri
mkdir -p "$CACHE"
G=https://ghproxy.net/https://github.com
R=https://ghproxy.net/https://raw.githubusercontent.com
echo "=== 1) AppRun-aarch64 ==="
curl -L -o "$CACHE/AppRun-aarch64" "$G/AppImage/AppImageKit/releases/download/continuous/AppRun-aarch64"
echo "=== 2) linuxdeploy-plugin-gtk.sh ==="
curl -L -o "$CACHE/linuxdeploy-plugin-gtk.sh" "$R/tauri-apps/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh"
echo "=== 3) linuxdeploy-aarch64.AppImage ==="
curl -L -o "$CACHE/linuxdeploy-aarch64.AppImage" "$G/tauri-apps/binary-releases/releases/download/linuxdeploy/linuxdeploy-aarch64.AppImage"
echo "=== sizes ==="
ls -lh "$CACHE/AppRun-aarch64" "$CACHE/linuxdeploy-plugin-gtk.sh" "$CACHE/linuxdeploy-aarch64.AppImage"
echo "FETCH_DONE rc=$?"
