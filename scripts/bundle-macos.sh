#!/bin/sh
# Build the macOS application bundle with BOTH binaries (design D10):
# lan-mouse (daemon / cli entry point) and lan-mouse-ui (the Go frontend),
# both in Contents/MacOS/. The default build bundles no GUI toolkit
# dylibs: the mygo frontend draws itself and links nothing from Homebrew.
# The accessibility grant attaches to the bundle, not to the daemon alone,
# because the daemon runs as a child of the bundled binary.
#
# The bundle is signed ad hoc so macOS treats it as a first-class app; TCC
# keys the accessibility grant to the bundle identifier and path, so the
# grant survives an in-place rebuild.
#
# usage: $0 [profile]    # debug (default) or release

set -e

usage() {
    sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

if [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
    usage
fi

profile="${1:-debug}"
version=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')

echo "building profile $profile ..."
if [ "$profile" = "debug" ]; then
    cargo build
else
    cargo build --profile "$profile"
fi

bundle="target/$profile/bundle/osx/Lan Mouse.app"
contents="$bundle/Contents"
macos_dir="$contents/MacOS"
resources="$contents/Resources"

mkdir -p "$macos_dir" "$resources"
cp "target/$profile/lan-mouse" "$macos_dir/"
cp "target/$profile/lan-mouse-ui" "$macos_dir/"

# the icns is optional; build it with scripts/makeicns.sh when wanted
icon_key=""
if [ -f target/icon.icns ]; then
    cp target/icon.icns "$resources/icon.icns"
    icon_key='	<key>CFBundleIconFile</key>
	<string>icon</string>'
fi

cat > "$contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>English</string>
	<key>CFBundleExecutable</key>
	<string>lan-mouse</string>
	<key>CFBundleIdentifier</key>
	<string>de.feschber.LanMouse</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Lan Mouse</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>CFBundleSignature</key>
	<string>????</string>
	<key>NSHighResolutionCapable</key>
	<true/>
$icon_key
	<key>LSUIElement</key>
	<true/>
	<key>NSAppSleepDisabled</key>
	<true/>
	<key>NSInputMonitoringUsageDescription</key>
	<string>Lan Mouse needs Input Monitoring access to capture keyboard and mouse input and forward it to remote machines on your network.</string>
	<key>NSAppleEventsUsageDescription</key>
	<string>Lan Mouse uses Apple Events to deliver synthesized keyboard and mouse events to the system.</string>
</dict>
</plist>
EOF

echo "ad hoc signing the bundle ..."
codesign --force --sign - "$bundle"

echo "bundle ready: $bundle"
