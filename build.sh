#!/usr/bin/env bash
set -euo pipefail

TARGET=aarch64-linux-android
CRATE_BIN=charger-hal-service
OUT=vendor.oplus.hardware.charger-V10-service

echo ">> cargo build --release --target ${TARGET}"
cargo build --release --target "${TARGET}"

SRC="target/${TARGET}/release/${CRATE_BIN}"
cp -f "${SRC}" "./${OUT}"

NDK="${NDK:-/opt/android-ndk}"
STRIP="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip"
[ -x "$STRIP" ] && "$STRIP" --strip-all \
  --remove-section=.comment \
  --remove-section=.note.gnu.gold-version \
  "./${OUT}" || true

echo
echo ">> DONE: ./${OUT}"
file "./${OUT}"
ls -lh "./${OUT}" | awk '{print ">> Size: "$5}'
echo ">> Check Symbol (NEEDED libs):"
"$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf" -d "./${OUT}" 2>/dev/null | grep -i needed || true
