#!/usr/bin/env bash
# Sestaví balíček .deb pro Ubuntu 24.04 a odvozené distribuce (Mint 22, Pop!_OS…).
#
# Použití:  packaging/build-deb.sh
# Výsledek: target/deb/tymy_<verze>_<arch>.deb
#
# Potřebuje: cargo, dpkg-dev (dpkg-deb, dpkg-shlibdeps); volitelně ImageMagick (PNG ikony).
# Správce balíčku: DEBFULLNAME / DEBEMAIL, jinak jméno a e-mail z git config.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
arch=$(dpkg --print-architecture)
name=${DEBFULLNAME:-$(git config user.name || echo "Tymy")}
email=${DEBEMAIL:-$(git config user.email || echo "tymy@localhost")}

echo "==> cargo build --release (tymy $version, $arch)"
cargo build --release --locked -p tymy -p tymy-cli

out=target/deb
stage=$out/tymy_${version}_${arch}
rm -rf "$stage"
mkdir -p "$stage/DEBIAN"

install -Dm755 target/release/tymy "$stage/usr/bin/tymy"
install -Dm755 target/release/tymy-cli "$stage/usr/bin/tymy-cli"
strip --strip-unneeded "$stage/usr/bin/tymy" "$stage/usr/bin/tymy-cli"

install -Dm644 packaging/tymy.desktop "$stage/usr/share/applications/tymy.desktop"
install -Dm644 packaging/tymy.svg "$stage/usr/share/icons/hicolor/scalable/apps/tymy.svg"
if command -v convert >/dev/null; then
    for size in 16 24 32 48 64 128 256; do
        dir="$stage/usr/share/icons/hicolor/${size}x${size}/apps"
        mkdir -p "$dir"
        convert -background none -density 384 packaging/tymy.svg \
            -resize "${size}x${size}" "$dir/tymy.png"
    done
else
    echo "    (ImageMagick chybí: jen SVG ikona)"
fi

doc="$stage/usr/share/doc/tymy"
install -Dm644 README.md "$doc/README.md"
cat >"$doc/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: tymy

Files: *
Copyright: $(date +%Y) $name
License: MIT
EOF

# Závislosti na sdílených knihovnách podle skutečně slinkovaných binárek.
shlib_dir=$(mktemp -d)
trap 'rm -rf "$shlib_dir"' EXIT
mkdir -p "$shlib_dir/debian"
echo "Source: tymy" >"$shlib_dir/debian/control"
depends=$(
    cd "$shlib_dir" &&
        dpkg-shlibdeps -O "-e$OLDPWD/$stage/usr/bin/tymy" "-e$OLDPWD/$stage/usr/bin/tymy-cli" 2>/dev/null |
        sed -n 's/^shlibs:Depends=//p'
)
# Okno (winit, wgpu) načítá knihovny X11/Wayland a grafiky až za běhu (dlopen), takže je
# dpkg-shlibdeps nevidí. Bez GPU ovladačů iced kreslí softwarově, proto jsou jen doporučené.
depends="$depends, libx11-6, libx11-xcb1, libxcursor1, libxi6, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, fonts-dejavu-core | fonts-noto-core"
recommends="xdg-utils, gnome-keyring | kwalletmanager, libvulkan1, mesa-vulkan-drivers | libegl1"
size=$(du -sk --exclude=DEBIAN "$stage" | cut -f1)

cat >"$stage/DEBIAN/control" <<EOF
Package: tymy
Version: $version
Architecture: $arch
Maintainer: $name <$email>
Installed-Size: $size
Depends: $depends
Recommends: $recommends
Section: net
Priority: optional
Description: Lehký klient Microsoft Teams pro Linux
 Nativní klient Microsoft Teams: chaty, kanály týmů, hledání ve zprávách,
 notifikace a ikona v liště. Používá interní API Teams web klienta.
 Projekt není nijak spojen s Microsoftem.
EOF

dpkg-deb --root-owner-group --build "$stage" "$out/tymy_${version}_${arch}.deb" >/dev/null
echo "==> $out/tymy_${version}_${arch}.deb"
echo "    Depends: $depends"
