#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"
bindir="$prefix/bin"
libdir="$prefix/lib/grimoir"
appdir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icondir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"

cd "$root"
cargo build --release -p grimoir
./scripts/fetch-pdfium.sh

install -Dm755 target/release/grimoir "$bindir/grimoir-real"
install -Dm644 vendor/pdfium/lib/libpdfium.so "$libdir/libpdfium.so"

cat > "$bindir/grimoir" <<EOF
#!/bin/sh
unset GIO_LAUNCHED_DESKTOP_FILE GIO_LAUNCHED_DESKTOP_FILE_PID DESKTOP_STARTUP_ID
if [ -d "$libdir" ]; then
  export GRIMOIR_PDFIUM_DIR="$libdir"
fi
exec "$bindir/grimoir-real" "\$@"
EOF
chmod 755 "$bindir/grimoir"

install -Dm644 assets/grimoir.svg "$icondir/grimoir.svg"
install -Dm644 assets/grimoir.desktop "$appdir/grimoir.desktop"
# Keep the public asset generic; the local entry uses this machine's prefix.
sed -i "s|^Exec=.*|Exec=$bindir/grimoir %f|" "$appdir/grimoir.desktop"

update-desktop-database "$appdir" 2>/dev/null || true
gtk-update-icon-cache -f -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" 2>/dev/null || true
printf 'Installed Grimoir to %s\n' "$prefix"
