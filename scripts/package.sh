#!/bin/sh
# Build a self-contained Linux x86_64 tarball.
# The bundled libpdfium.so sits next to the binary, which is one of the
# locations checked by grimoir-pdf at startup.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

cargo build --release -p grimoir
./scripts/fetch-pdfium.sh

out="$root/dist/grimoir"
rm -rf "$out"
mkdir -p "$out"

install -m755 target/release/grimoir "$out/grimoir"
install -m755 vendor/pdfium/lib/libpdfium.so "$out/libpdfium.so"
install -m644 assets/grimoir.desktop "$out/grimoir.desktop"
install -m644 assets/grimoir.svg "$out/grimoir.svg"
install -m644 LICENSE "$out/LICENSE"

cat >"$out/README.txt" <<'EOF'
Grimoir — portable Linux build
==============================

Run ./grimoir to open the library.
Run ./grimoir /path/to/book.pdf to open a book directly.

libpdfium.so must stay next to the grimoir binary.
EOF

tar -C "$root/dist" -czf "$root/dist/grimoir-linux-x86_64.tar.gz" grimoir
printf 'wrote %s\n' "$root/dist/grimoir-linux-x86_64.tar.gz"
