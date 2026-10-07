# Only built if the actual base AT-SPI is older than Orca 51 requires.
pkgname=at-spi2-core
pkgver=2.58.9
pkgrel=1
pkgdesc='AT-SPI accessibility stack (pinned upstream CI qualification)'
arch=('x86_64')
url='https://gitlab.gnome.org/GNOME/at-spi2-core'
license=('LGPL-2.1-or-later')
depends=('dbus' 'glib2' 'libx11' 'libxtst' 'libxi' 'libxml2' 'systemd-libs' 'python-gobject')
makedepends=('meson' 'pkgconf' 'gobject-introspection' 'glib2-devel')
provides=('libatk-1.0.so' 'libatk-bridge-2.0.so' 'libatspi.so')
options=('!debug')
source=("https://download.gnome.org/sources/at-spi2-core/2.58/at-spi2-core-$pkgver.tar.xz")
sha256sums=('c8eacbe2640038178f2c2cd7abef2c23c7a4777909119f9d815c7151b39fb82a')
build() {
    meson setup "$srcdir/at-spi2-core-$pkgver" "$srcdir/build" --prefix=/usr --libdir=lib --sysconfdir=/etc -Ddocs=false -Dintrospection=enabled -Dx11=enabled
    meson compile -C "$srcdir/build"
}
package() {
    DESTDIR="$pkgdir" meson install -C "$srcdir/build"
}
