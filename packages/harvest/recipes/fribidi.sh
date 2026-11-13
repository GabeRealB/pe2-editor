URL=https://github.com/fribidi/fribidi/releases/download/v1.0.18/fribidi-1.0.18.tar.xz
PORT=fribidi; ARCHIVES=lib/libfribidi.a=fribidi
configure() { meson_configure -Ddocs=false -Dbin=false -Dtests=false; }
make_it() { meson_make; }; install() { meson_install; }
