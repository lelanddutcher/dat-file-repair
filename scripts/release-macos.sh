#!/bin/bash
# Build, sign, notarize and staple the DATfix macOS app on a Mac.
#
# Why not in GitHub Actions: signing and notarization need a Developer ID certificate and a
# notary credential, which stay on the maintainer's machine. Stapling needs no credential.
#
# Usage: scripts/release-macos.sh [out-dir]          (default out-dir: dist/)
#
# Environment:
#   APPLE_SIGNING_IDENTITY  required. The codesign identity, for example
#                           "Developer ID Application: <Name> (<TEAMID>)". The certificate must
#                           be in the login keychain.
#   DATFIX_NOTARY_PROFILE   required. A `notarytool store-credentials` keychain profile name.
#   DATFIX_NOTARYTOOL       optional. A command that takes `notarytool` arguments (default:
#                           `xcrun notarytool`). Set it to a wrapper script to notarize elsewhere.
#
# Run from a GUI-session shell (codesign may need keychain access).
# Optional: upload to a draft release with
#   gh release upload v<version> <out-dir>/DATfix_<version>_macos_universal.dmg --clobber
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/dist}"
IDENTITY="${APPLE_SIGNING_IDENTITY:?set APPLE_SIGNING_IDENTITY to your Developer ID Application identity}"
PROFILE="${DATFIX_NOTARY_PROFILE:?set DATFIX_NOTARY_PROFILE to a notarytool keychain profile}"
NOTARY="${DATFIX_NOTARYTOOL:-}"
VERSION="$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$ROOT/app/package.json" | head -1)"
[ -n "$VERSION" ] || { echo "could not read the version from app/package.json" >&2; exit 1; }
TARGET=universal-apple-darwin
BUNDLE="$ROOT/target/$TARGET/release/bundle"
FINAL="$OUT/DATfix_${VERSION}_macos_universal.dmg"

step() { printf '\n== %s\n' "$*"; }

step "Build and sign DATfix $VERSION ($IDENTITY)"
rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
(cd "$ROOT/app" && npm ci --silent)
# CI=true skips Finder's DMG layout script, which needs an automation permission.
(cd "$ROOT/app" && APPLE_SIGNING_IDENTITY="$IDENTITY" CI=true cargo tauri build --target "$TARGET")

APP="$BUNDLE/macos/DATfix.app"
DMG="$(ls "$BUNDLE"/dmg/DATfix_*.dmg | head -1)"
codesign --verify --deep --strict "$APP"
SIGINFO="$(codesign -dv "$APP" 2>&1)"
[[ "$SIGINFO" == *"(runtime)"* ]] || { echo "hardened runtime missing" >&2; exit 1; }
codesign --verify "$DMG"

mkdir -p "$OUT"
cp "$DMG" "$FINAL"

if [ -n "$NOTARY" ]; then
  step "Notarize via $(basename "$NOTARY") (profile $PROFILE)"
  bash "$NOTARY" submit "$FINAL" --keychain-profile "$PROFILE" --wait --timeout 30m
else
  step "Notarize via xcrun notarytool (profile $PROFILE)"
  xcrun notarytool submit "$FINAL" --keychain-profile "$PROFILE" --wait --timeout 30m
fi

step "Staple and verify"
xcrun stapler staple "$FINAL"
DMGCHECK="$(spctl -a -vv -t open --context context:primary-signature "$FINAL" 2>&1)"; echo "$DMGCHECK"
[[ "$DMGCHECK" == *"Notarized Developer ID"* ]]
MNT="$(hdiutil attach -readonly -nobrowse "$FINAL" | awk -F'\t' '/\/Volumes\//{print $NF}')"
trap 'hdiutil detach -quiet "$MNT" 2>/dev/null || true' EXIT
APPCHECK="$(spctl -a -vv "$MNT/DATfix.app" 2>&1)"; echo "$APPCHECK"
[[ "$APPCHECK" == *"Notarized Developer ID"* ]]
hdiutil detach -quiet "$MNT"; trap - EXIT

(cd "$OUT" && shasum -a 256 "$(basename "$FINAL")")
echo "Done: $FINAL"
