#!/bin/sh
# Make Tang.clap, the file that a DAW loads, from the library that cargo built (macOS and Linux; on Windows the library, tang_clap.dll, is the file: copy it as Tang.clap, see docs/reference/plugin-host.md).
#
#   cargo build --release -p tang-clap
#   tang-clap/bundle.sh release [directory]       # Tang.clap in that directory (target/bundle by default)
#
# On macOS a .clap is a bundle: a directory with the library in Contents/MacOS and an Info.plist; on Linux it is the library itself.
set -eu

profile="${1:-release}"
here="$(cd "$(dirname "$0")/.." && pwd)"
out="${2:-$here/target/bundle}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/tang-clap/Cargo.toml" | head -1)"
mkdir -p "$out"
rm -rf "$out/Tang.clap"

case "$(uname -s)" in
  Darwin)
    library="$here/target/$profile/libtang_clap.dylib"
    [ -f "$library" ] || { echo "no $library: run cargo build -p tang-clap$([ "$profile" = release ] && echo ' --release') first" >&2; exit 1; }
    mkdir -p "$out/Tang.clap/Contents/MacOS"
    cp "$library" "$out/Tang.clap/Contents/MacOS/Tang"
    cat > "$out/Tang.clap/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>Tang</string>
  <key>CFBundleIdentifier</key><string>io.github.apanloco.tang</string>
  <key>CFBundleName</key><string>Tang</string>
  <key>CFBundlePackageType</key><string>BNDL</string>
  <key>CFBundleSignature</key><string>????</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
</dict>
</plist>
PLIST
    # A signature of no identity (ad hoc), which makes the bundle whole: its plist is bound to it and `codesign --verify --strict` passes. It is enough for the Mac that built it; another Mac that is strict about plugins needs a bundle that is signed and notarised.
    if command -v codesign >/dev/null 2>&1; then
      codesign --force --sign - --timestamp=none "$out/Tang.clap" >/dev/null 2>&1 || echo "warning: $out/Tang.clap could not be signed (ad hoc)" >&2
    fi
    ;;
  *)
    library="$here/target/$profile/libtang_clap.so"
    [ -f "$library" ] || { echo "no $library: run cargo build -p tang-clap first" >&2; exit 1; }
    cp "$library" "$out/Tang.clap"
    ;;
esac
echo "$out/Tang.clap"
