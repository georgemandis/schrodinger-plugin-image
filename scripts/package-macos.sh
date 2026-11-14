#!/bin/sh
# Build the plugin and stage it for install: dist/image/{plugin.json,lib/}.
#
# The plugin links loupe as a dynamic library. Other Schrödinger plugins
# (OCR, Faces) bundle their own copy, possibly an older one, and all plugins
# load into one process. macOS matches a library that is already loaded by
# its install name, so a second plugin asking for "@rpath/libloupe.dylib"
# is handed the first plugin's copy, whatever its version. To stay
# independent, this plugin's copy gets its own name.
#
# The name must be no longer than the original by more than two characters:
# loupe's release library is linked without spare header space, so
# install_name_tool can only write a name that fits the existing slot.
set -eu
cd "$(dirname "$0")/.."

cargo build --release

OUT=dist/image
PLUGIN=libschrodinger_plugin_image.dylib
LOUPE=loupe-img.dylib

rm -rf "$OUT"
mkdir -p "$OUT/lib"
cp plugin.json "$OUT/"
cp "target/release/$PLUGIN" "$OUT/lib/$PLUGIN"

# Where the loupe-rs build script put libloupe. Asked of cargo, because newer
# cargo keeps build scripts' output outside target/.
LOUPE_OUT=$(cargo build --release --message-format=json 2>/dev/null | python3 -c '
import json, sys
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "build-script-executed" and "loupe-rs" in m.get("package_id", ""):
        print(m["out_dir"])
')
SRC="$LOUPE_OUT/libloupe.dylib"
[ -f "$SRC" ] || { echo "libloupe.dylib not found (looked in: $LOUPE_OUT)" >&2; exit 1; }
cp "$SRC" "$OUT/lib/$LOUPE"

install_name_tool -id "@rpath/$LOUPE" "$OUT/lib/$LOUPE"
install_name_tool -change @rpath/libloupe.dylib "@rpath/$LOUPE" "$OUT/lib/$PLUGIN"
install_name_tool -add_rpath @loader_path "$OUT/lib/$PLUGIN"

# Editing a signed library invalidates its signature; sign again (ad hoc
# unless CODESIGN_IDENTITY names a certificate).
codesign --force --sign "${CODESIGN_IDENTITY:--}" "$OUT/lib/$LOUPE" "$OUT/lib/$PLUGIN"

echo "Staged in $OUT. Install with:"
echo "  rsync -a --delete $OUT/ ~/.schrodinger/plugins/image/"
