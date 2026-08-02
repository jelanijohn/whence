#!/usr/bin/env bash
# Package the browser extension for distribution (docs/extension-distribution.md §7):
# zip extension/ into whence-browser-extension-v<version>.zip, the artifact for both the
# Chrome Web Store upload and the load-unpacked fallback attached to GitHub
# releases. The version is read from manifest.json so the two cannot drift.
set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

ROOT=$(cd "$(dirname "$0")/.." && pwd)
EXT="$ROOT/extension"
MANIFEST="$EXT/manifest.json"

[[ -f "$MANIFEST" ]] || die "no manifest at $MANIFEST"
command -v zip >/dev/null || die "zip is not installed"

# The one version source. jq-free on purpose — a release helper shouldn't gate on
# extra tooling; the tripwires already pin the manifest's shape.
VERSION=$(sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST")
[[ -n "$VERSION" ]] || die "could not read \"version\" from $MANIFEST"

OUT="$ROOT/whence-browser-extension-v$VERSION.zip"
rm -f "$OUT"

# Package contents = exactly what the browser loads: manifest, scripts, options
# page, icons. README and any dotfiles are repo furniture, not extension code.
(cd "$EXT" && zip -r -X "$OUT" . -x "README.md" -x ".*" -x "*/.*")

echo
echo "Packaged $OUT"
echo "  - Chrome Web Store: upload this zip on the developer dashboard"
echo "  - GitHub release:   attach as the load-unpacked fallback"
