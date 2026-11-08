#!/bin/sh
# Builds "UA-101 Panel.app" (universal x86_64 + arm64) and a .dmg.
#
#   packaging/macos/make-app.sh <version> <out-dir>
#
# Expects release binaries in target/{x86_64,aarch64}-apple-darwin/release/.
set -eu

VERSION="${1:?version}"
OUT="${2:?output directory}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP="$OUT/UA-101 Panel.app"

mkdir -p "$OUT"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

for bin in ua101-panel ua101; do
    lipo -create \
        "$ROOT/target/x86_64-apple-darwin/release/$bin" \
        "$ROOT/target/aarch64-apple-darwin/release/$bin" \
        -output "$APP/Contents/MacOS/$bin"
done
sed "s/@VERSION@/${VERSION#v}/g" "$ROOT/packaging/macos/Info.plist" > "$APP/Contents/Info.plist"

# Ad-hoc signature so Apple Silicon accepts the universal binary.
codesign --force --deep --sign - "$APP"

# Stand-alone universal CLI next to the app.
cp "$APP/Contents/MacOS/ua101" "$OUT/ua101"

STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "UA-101 Panel" -srcfolder "$STAGE" -ov -format UDZO \
    "$OUT/ua101-macos-universal.dmg"
rm -rf "$STAGE"
(cd "$OUT" && zip -qry ua101-macos-universal-app.zip "UA-101 Panel.app")
tar -czf "$OUT/ua101-macos-universal-cli.tar.gz" -C "$OUT" ua101
echo "built $APP"
