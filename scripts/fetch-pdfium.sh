#!/usr/bin/env bash
# Downloads a prebuilt PDFium binary for the current Linux architecture.
# The binary is BSD-licensed; it is intentionally not committed to this repo.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PDFIUM_VERSION="${PDFIUM_VERSION:-8125}"
DEST="${PDFIUM_DEST:-$ROOT/vendor/pdfium}"

case "$(uname -m)" in
  x86_64|amd64) ARCH="x64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  *)
    echo "Unsupported architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

URL="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/${PDFIUM_VERSION}/pdfium-linux-${ARCH}.tgz"

if [[ "${1:-}" != "--force" && -f "$DEST/lib/libpdfium.so" ]]; then
  echo "PDFium already present at $DEST (use --force to re-download)."
  exit 0
fi

echo "Downloading PDFium ${PDFIUM_VERSION} for ${ARCH}..."
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fL --retry 3 --progress-bar "$URL" -o "$TMP/pdfium.tgz"
mkdir -p "$DEST"
tar -xzf "$TMP/pdfium.tgz" -C "$DEST"

if [[ ! -f "$DEST/lib/libpdfium.so" ]]; then
  echo "PDFium extraction did not produce $DEST/lib/libpdfium.so" >&2
  exit 1
fi

echo "PDFium ready at $DEST"
