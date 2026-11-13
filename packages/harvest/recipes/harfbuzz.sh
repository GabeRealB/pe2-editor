URL=https://github.com/harfbuzz/harfbuzz/releases/download/15.1.0/harfbuzz-15.1.0.tar.xz
PORT=harfbuzz; ARCHIVES=src/libharfbuzz.a=harfbuzz
configure() { meson_configure -Dfreetype=enabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dchafa=disabled -Dicu=disabled -Dgraphite2=disabled -Dtests=disabled -Ddocs=disabled -Dutilities=disabled -Dbenchmark=disabled -Dintrospection=disabled -Dsubset=disabled -Draster=disabled -Dvector=disabled; }
make_it() { meson_make; }
# It is C++, which its .pc file does not say for a static link.
install() { meson_install && sed -i 's/^\(Libs: .*-lharfbuzz.*\)$/\1 -lc++/' $SYS/lib/pkgconfig/harfbuzz.pc; }
