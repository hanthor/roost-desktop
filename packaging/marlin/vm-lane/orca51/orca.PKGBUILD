# CI-only genuine upstream package; never published to the production image.
pkgname=orca
pkgver=51.0
pkgrel=1
pkgdesc='GNOME screen reader (pinned upstream CI qualification)'
arch=('any')
url='https://gitlab.gnome.org/GNOME/orca'
license=('LGPL-2.1-or-later')
depends=('at-spi2-core>=2.58.6' 'gtk3' 'python-gobject' 'python-dasbus' 'speech-dispatcher')
makedepends=('meson' 'gettext' 'itstool' 'libxslt' 'docbook-xsl' 'pkgconf')
options=('!debug')
source=("https://download.gnome.org/sources/orca/51/orca-$pkgver.tar.xz")
sha256sums=('8bc3e44bc5b7b66ec7e0cc5c82695c0075661922745d25724f5fc84a25602108')
build() {
    meson setup "$srcdir/orca-$pkgver" "$srcdir/build" --prefix=/usr --libdir=lib --sysconfdir=/etc -Dmathcat=false -Dspiel=false
    meson compile -C "$srcdir/build"
}
package() {
    DESTDIR="$pkgdir" meson install -C "$srcdir/build"
}
