#!/usr/bin/env bash
set -euo pipefail

# Run from the source checkout after cargo build --release.
version=$1
platform=$2
arch=$3
binary="$(pwd)/target/release/capterm"
mkdir -p dist
dist="$(pwd)/dist"
work=$(mktemp -d)
mount="$work/mounted"
mounted=false
cleanup() {
    if "$mounted"; then
        hdiutil detach "$mount"
    fi
    rm -rf -- "$work"
}
trap cleanup EXIT

copy_licenses() {
    mkdir -p "$1"
    install -m 644 LICENSE "$1/CapTerm-Apache-2.0.txt"
    install -m 644 assets/fonts/*-OFL.txt assets/licenses/Lucide-Feather.txt "$1/"
}

case "$platform" in
    macos)
        lipo -verify_arch "$arch" "$binary"
        app="$work/image/CapTerm.app"
        mkdir -p "$app/Contents/"{MacOS,Resources}
        install -m 755 "$binary" "$app/Contents/MacOS/capterm"
        copy_licenses "$app/Contents/Resources/licenses"

        iconset="$work/CapTerm.iconset"
        mkdir "$iconset"
        for size in 16 32 128 256 512; do
            sips -z "$size" "$size" design/icon/capterm.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
            sips -z "$((size * 2))" "$((size * 2))" design/icon/capterm.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
        done
        iconutil -c icns "$iconset" -o "$app/Contents/Resources/CapTerm.icns"

        cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDisplayName</key><string>CapTerm</string>
    <key>CFBundleName</key><string>CapTerm</string>
    <key>CFBundleExecutable</key><string>capterm</string>
    <key>CFBundleIdentifier</key><string>io.github.yulianjie.capterm</string>
    <key>CFBundleIconFile</key><string>CapTerm.icns</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
        plutil -lint "$app/Contents/Info.plist"
        codesign --force --sign - "$app"
        ln -s /Applications "$work/image/Applications"
        dmg="$dist/CapTerm-$version-macos-$arch.dmg"
        hdiutil create -volname CapTerm -srcfolder "$work/image" -format UDZO -ov "$dmg"
        hdiutil verify "$dmg"
        hdiutil attach "$dmg" -nobrowse -readonly -mountpoint "$mount"
        mounted=true
        codesign --verify --deep --strict --verbose "$mount/CapTerm.app"
        cmp "$app/Contents/MacOS/capterm" "$mount/CapTerm.app/Contents/MacOS/capterm"
        ;;
    linux)
        test "$(dpkg --print-architecture)" = "$arch"
        stage="$work/debian/capterm"
        install -Dm755 "$binary" "$stage/usr/bin/capterm"
        install -Dm644 design/icon/capterm.svg "$stage/usr/share/icons/hicolor/scalable/apps/capterm.svg"
        copy_licenses "$stage/usr/share/doc/capterm"
        mv "$stage/usr/share/doc/capterm/CapTerm-Apache-2.0.txt" "$stage/usr/share/doc/capterm/copyright"

        mkdir -p "$stage/usr/share/applications"
        cat > "$stage/usr/share/applications/capterm.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=CapTerm
Comment=SSH workbench
Exec=capterm
Icon=capterm
Terminal=false
Categories=System;TerminalEmulator;
EOF

        mkdir -p "$work/debian" "$stage/DEBIAN"
        cat > "$work/debian/control" <<EOF
Source: capterm

Package: capterm
Architecture: any
EOF
        depends=$(cd "$work" && dpkg-shlibdeps -O -edebian/capterm/usr/bin/capterm)
        cat > "$stage/DEBIAN/control" <<EOF
Package: capterm
Version: $version
Architecture: $arch
Section: utils
Priority: optional
Maintainer: CapTerm maintainers <34149758+yulianjie@users.noreply.github.com>
Depends: ${depends#shlibs:Depends=}, libvulkan1, libwayland-client0, xdg-utils
Recommends: mesa-vulkan-drivers
Homepage: https://github.com/yulianjie/capterm
Description: Native SSH workbench with SFTP and host monitoring
EOF
        deb="$dist/CapTerm-$version-linux-$arch.deb"
        dpkg-deb --root-owner-group --build "$stage" "$deb"
        sudo apt-get install -y "$deb"
        cmp "$binary" /usr/bin/capterm
        env -u WAYLAND_DISPLAY xvfb-run -a bash -euc '
            capterm &
            pid=$!
            trap "kill $pid 2>/dev/null || true; wait $pid 2>/dev/null || true" EXIT
            timeout 30s xdotool search --sync --onlyvisible --name "^CapTerm$" >/dev/null
            kill -0 "$pid"
        '
        ;;
    *)
        echo "Unsupported packaging platform: $platform" >&2
        exit 1
        ;;
esac
