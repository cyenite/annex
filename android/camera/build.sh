#!/usr/bin/env bash
# Rebuilds android.hardware.camera.provider-service.annex, the camera HAL that annex mounts into
# container-mode instances. The binary is committed so building annex never needs the NDK; run
# this after changing camera.cpp and commit the result.
#
# Needs the Android NDK, build-tools 35.0.0 (its aidl matches Android 15's binder headers),
# Docker with the Android 15 image pulled, and access to android.googlesource.com and GitHub.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
sdk=${ANDROID_HOME:-$HOME/Android/Sdk}
ndk=${ANDROID_NDK:-$sdk/ndk/29.0.14206865}
aidl=$sdk/build-tools/35.0.0/aidl
image=redroid/redroid:15.0.0-latest
aosp=https://android.googlesource.com/platform
interfaces=$aosp/hardware/interfaces/+archive/refs/heads/android15-release
stb=https://raw.githubusercontent.com/nothings/stb/d14d46f6b44456eb88beac58cba833a4135d4c89
work=$(mktemp -d)
trap 'rm -rf "$work"; docker rm -f annex-camera-build >/dev/null 2>&1 || true' EXIT

mkdir -p "$work"/{api,gen,lib,meta,stb,shim/log,shim/cutils,shim/vndk}
mkdir -p "$work"/hdr/{include_cpp,include_ndk,include_platform,bionic/android}
# The versions camera.provider V1 was frozen against; V1 is the oldest AIDL camera HAL, so the
# framework of every Android version with AIDL cameras accepts it.
packages=(
  camera/provider:android.hardware.camera.provider:1
  camera/device:android.hardware.camera.device:1
  camera/common:android.hardware.camera.common:1
  camera/metadata:android.hardware.camera.metadata:1
  common:android.hardware.common:2
  common/fmq:android.hardware.common.fmq:1
  graphics/common:android.hardware.graphics.common:5
)
includes=()
for package in "${packages[@]}"; do
  IFS=: read -r dir name version <<<"$package"
  mkdir -p "$work/api/$name"
  curl -sfL "$interfaces/$dir/aidl/aidl_api/$name/$version.tar.gz" | tar -xz -C "$work/api/$name"
  includes+=(-I "$work/api/$name")
done
for package in "${packages[@]}"; do
  IFS=: read -r _ name version <<<"$package"
  mapfile -t sources < <(find "$work/api/$name" -name '*.aidl')
  "$aidl" --lang=ndk --stability=vintf --structured --version="$version" \
    --hash="$(tail -n 1 "$work/api/$name/.hash")" "${includes[@]}" -h "$work/gen/include" -o "$work/gen/src" \
    "${sources[@]}"
done

for dir in include_cpp include_ndk include_platform; do
  curl -sfL "$aosp/frameworks/native/+archive/refs/heads/android15-release/libs/binder/ndk/$dir.tar.gz" |
    tar -xz -C "$work/hdr/$dir"
done
# binder_manager.h includes this bionic header only for its availability annotations.
printf '#pragma once\n#define __INTRODUCED_IN_LLNDK(level)\n' >"$work/hdr/bionic/android/llndk-versioning.h"

# Camera metadata is a packed buffer in the format libcamera_metadata defines; vendor processes
# can't link the system's copy, so its source is compiled in.
curl -sfL "$aosp/system/media/+archive/refs/heads/android15-release/camera.tar.gz" | tar -xz -C "$work/meta"
curl -sfL "$aosp/system/media/+archive/refs/heads/android15-release/private/camera/include.tar.gz" |
  tar -xz -C "$work/meta/include"
printf '#pragma once\n#define ANDROID_API\n' >"$work/shim/cutils/compiler.h"
printf '#pragma once\n#include <android/log.h>\n#define ALOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)\n#define android_errorWriteLog(tag, subtag) ((void)0)\n' \
  >"$work/shim/log/log.h"
curl -sfL "$aosp/frameworks/native/+/refs/heads/android15-release/libs/nativewindow/include/vndk/hardware_buffer.h?format=TEXT" |
  base64 -d >"$work/shim/vndk/hardware_buffer.h"
curl -sfL "$aosp/system/core/+/refs/heads/android15-release/libcutils/include/cutils/native_handle.h?format=TEXT" |
  base64 -d >"$work/shim/cutils/native_handle.h"
curl -sfL "$stb/stb_image_write.h" -o "$work/stb/stb_image_write.h"

# The NDK's stubs lack the vendor-only binder symbols and AHardwareBuffer_createFromHandle, so
# link against the image's libraries.
docker create --name annex-camera-build "$image" >/dev/null
docker cp annex-camera-build:/system/lib64/libbinder_ndk.so "$work/lib/" >/dev/null
docker cp annex-camera-build:/system/lib64/libnativewindow.so "$work/lib/" >/dev/null

bin=$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
"$bin/clang" --target=x86_64-linux-android34 -O2 -c -I"$work/meta/include" -I"$work/shim" \
  "$work/meta/src/camera_metadata.c" -o "$work/camera_metadata.o"
mapfile -t generated < <(find "$work/gen/src" -name '*.cpp')
"$bin/clang++" --target=x86_64-linux-android34 -std=c++17 -O2 -D__ANDROID_VENDOR__ \
  -I"$work/hdr/include_cpp" -I"$work/hdr/include_ndk" -I"$work/hdr/include_platform" -I"$work/hdr/bionic" \
  -I"$work/gen/include" -I"$work/meta/include" -I"$work/shim" -I"$work/stb" \
  "${generated[@]}" "$here/camera.cpp" "$work/camera_metadata.o" \
  -o "$here/android.hardware.camera.provider-service.annex" -static-libstdc++ -L"$work/lib" \
  -lbinder_ndk -lnativewindow -llog -s
echo "built $here/android.hardware.camera.provider-service.annex"
