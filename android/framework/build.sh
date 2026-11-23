#!/usr/bin/env bash
# Rebuilds AnnexSignatures.smali, the class annex adds to an instance's services.jar for the
# Google services stand-in. The smali is committed so annex never needs a JDK on the host; run
# this after changing AnnexSignatures.java and commit the result.
#
# Needs a JDK (17 or newer), and the Android SDK with platform 35 and build-tools 35.0.0.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
sdk=${ANDROID_HOME:-$HOME/Android/Sdk}
platform=$sdk/platforms/android-35/android.jar
d8=$sdk/build-tools/35.0.0/d8
apktool_url=https://github.com/iBotPeaches/Apktool/releases/download/v3.0.3/apktool_3.0.3.jar
apktool_sha256=dbf930b076c6b9be08d57c449cacefc3bdd6b71ebd59b3066fc0e1f5b14f9423
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

curl -sfLo "$work/apktool.jar" "$apktool_url"
echo "$apktool_sha256  $work/apktool.jar" | sha256sum -c --quiet -
mkdir "$work/classes" "$work/dex"
javac -nowarn -source 11 -target 11 -cp "$platform" -d "$work/classes" "$here/AnnexSignatures.java" 2>&1 |
  grep -v '^warning: \[options\]' || true
"$d8" --min-api 33 --lib "$platform" --output "$work/dex" "$work/classes/com/android/server/pm/AnnexSignatures.class"
(cd "$work/dex" && zip -q helper.jar classes.dex)
java -jar "$work/apktool.jar" d -f -o "$work/smali" "$work/dex/helper.jar" >/dev/null
cp "$work/smali/smali/com/android/server/pm/AnnexSignatures.smali" "$here/AnnexSignatures.smali"
echo "built $here/AnnexSignatures.smali"
