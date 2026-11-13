URL=https://gitlab.freedesktop.org/api/v4/projects/890/packages/generic/fontconfig/2.18.3/fontconfig-2.18.3.tar.xz
PORT=fontconfig; ARCHIVES=libfontconfig.a=fontconfig
# Its configuration where systems keep it (/etc/fonts), as distributions build it.
configure() { meson_configure -Dsysconfdir=/etc -Dlocalstatedir=/var -Ddoc=disabled -Dtests=disabled -Dtools=disabled -Dcache-build=disabled -Dnls=disabled -Dxml-backend=expat; }
make_it() { meson_make; }; install() { DESTDIR= $MESON install --no-rebuild --skip-subprojects || true; }
