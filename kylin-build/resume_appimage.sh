#!/usr/bin/env bash
CACHE=/home/sheng/.cache/tauri
URL="https://ghproxy.net/https://github.com/tauri-apps/binary-releases/releases/download/linuxdeploy/linuxdeploy-aarch64.AppImage"
echo "resume start: $(stat -c%s "$CACHE/linuxdeploy-aarch64.AppImage" 2>/dev/null) bytes"
for i in $(seq 1 8); do
  echo "--- attempt $i ---"
  curl -L -C - --retry 5 --retry-delay 3 -o "$CACHE/linuxdeploy-aarch64.AppImage" "$URL" 2>&1 | tr '\r' '\n' | tail -n 1
  SZ=$(stat -c%s "$CACHE/linuxdeploy-aarch64.AppImage" 2>/dev/null)
  echo "size now: $SZ"
  if [ "$SZ" -ge 9700000 ]; then echo "DONE"; break; fi
  sleep 2
done
ls -lh "$CACHE/linuxdeploy-aarch64.AppImage"
