#!/usr/bin/env bash
# Rebuilds android.hardware.gnss-service.annex, the GNSS HAL that annex mounts into every
# container-mode instance. The binary is committed so building annex never needs the NDK; run
# this after changing gnss.cpp and commit the result.
#
# Needs the Android NDK, build-tools 35.0.0 (its aidl matches Android 15's binder headers),
# Docker with the Android 15 image pulled, and access to android.googlesource.com.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
sdk=${ANDROID_HOME:-$HOME/Android/Sdk}
ndk=${ANDROID_NDK:-$sdk/ndk/29.0.14206865}
aidl=$sdk/build-tools/35.0.0/aidl
image=redroid/redroid:15.0.0-latest
aosp=https://android.googlesource.com/platform
work=$(mktemp -d)
trap 'rm -rf "$work"; docker rm -f annex-gnss-build >/dev/null 2>&1 || true' EXIT

mkdir -p "$work"/{api,gen,lib,hdr/include_cpp,hdr/include_ndk,hdr/include_platform,hdr/bionic/android}
curl -sfL "$aosp/hardware/interfaces/+archive/refs/heads/android15-release/gnss/aidl/aidl_api/android.hardware.gnss/3.tar.gz" |
  tar -xz -C "$work/api"
for dir in include_cpp include_ndk include_platform; do
  curl -sfL "$aosp/frameworks/native/+archive/refs/heads/android15-release/libs/binder/ndk/$dir.tar.gz" |
    tar -xz -C "$work/hdr/$dir"
done
# binder_manager.h includes this bionic header only for its availability annotations.
printf '#pragma once\n#define __INTRODUCED_IN_LLNDK(level)\n' >"$work/hdr/bionic/android/llndk-versioning.h"

(cd "$work/api" && "$aidl" --lang=ndk --stability=vintf --structured --version=3 --hash="$(cat .hash)" \
  -I . -h "$work/gen/include" -o "$work/gen/src" $(find android -name '*.aidl'))

# The NDK's libbinder_ndk stub lacks the vendor-only stability symbols, so link against the image's.
docker create --name annex-gnss-build "$image" >/dev/null
docker cp annex-gnss-build:/system/lib64/libbinder_ndk.so "$work/lib/" >/dev/null

"$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/clang++" --target=x86_64-linux-android34 -std=c++17 -O2 \
  -D__ANDROID_VENDOR__ -I"$work/hdr/include_cpp" -I"$work/hdr/include_ndk" -I"$work/hdr/include_platform" \
  -I"$work/hdr/bionic" -I"$work/gen/include" $(find "$work/gen/src" -name '*.cpp') "$here/gnss.cpp" \
  -o "$here/android.hardware.gnss-service.annex" -static-libstdc++ -L"$work/lib" -lbinder_ndk -llog -s
echo "built $here/android.hardware.gnss-service.annex"
