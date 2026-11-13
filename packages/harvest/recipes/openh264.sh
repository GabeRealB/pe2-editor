URL=https://github.com/cisco/openh264/archive/refs/tags/v2.7.0.tar.gz
PORT=openh264; ARCHIVES=libopenh264.a=openh264
configure() { meson_configure -Dtests=disabled; }
make_it() { meson_make; }
# It is C++, which its .pc file does not say for a static link.
install() { meson_install && sed -i 's/^\(Libs: .*-lopenh264.*\)$/\1\nLibs.private: -lc++/' $SYS/lib/pkgconfig/openh264.pc; }
