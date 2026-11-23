#!/usr/bin/env bash
# Rebuilds the two files annex embeds for the Google services stand-in, both committed so annex
# never needs a JDK on the host; run this after changing either .java file and commit the result:
# - AnnexPush.smali, the offline push token handler annex adds to microG
# - annex-sign.jar, the APK signer annex runs (with apksig) to re-sign the patched microG
#
# Needs a JDK (17 or newer), and the Android SDK with platform 35 and build-tools 35.0.0.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
sdk=${ANDROID_HOME:-$HOME/Android/Sdk}
platform=$sdk/platforms/android-35/android.jar
d8=$sdk/build-tools/35.0.0/d8
apktool_url=https://github.com/iBotPeaches/Apktool/releases/download/v3.0.3/apktool_3.0.3.jar
apktool_sha256=dbf930b076c6b9be08d57c449cacefc3bdd6b71ebd59b3066fc0e1f5b14f9423
apksig_url=https://dl.google.com/android/maven2/com/android/tools/build/apksig/9.4.1/apksig-9.4.1.jar
apksig_sha256=7ae2e5980c77d853e3513074ee7c822bbdcdcde1668889d17faa7fe8bc8aa821
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() {
  curl -sfLo "$work/$1" "$2"
  echo "$3  $work/$1" | sha256sum -c --quiet -
}
fetch apktool.jar "$apktool_url" "$apktool_sha256"
fetch apksig.jar "$apksig_url" "$apksig_sha256"

mkdir "$work/push" "$work/dex" "$work/sign"
javac -nowarn -source 11 -target 11 -cp "$platform" -d "$work/push" "$here/AnnexPush.java" 2>&1 |
  grep -v '^warning: \[options\]' || true
"$d8" --min-api 23 --lib "$platform" --output "$work/dex" $(find "$work/push" -name '*.class')
(cd "$work/dex" && zip -q push.jar classes.dex)
java -jar "$work/apktool.jar" d -f -o "$work/smali" "$work/dex/push.jar" >/dev/null
cp "$work/smali/smali/dev/annex/AnnexPush.smali" "$here/AnnexPush.smali"

javac -nowarn --release 17 -cp "$work/apksig.jar" -d "$work/sign" "$here/AnnexSign.java"
jar --create --file "$here/annex-sign.jar" --date 2026-01-01T00:00:00Z -C "$work/sign" .
echo "built $here/AnnexPush.smali and $here/annex-sign.jar"
