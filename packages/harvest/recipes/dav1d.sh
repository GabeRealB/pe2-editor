URL=https://downloads.videolan.org/pub/videolan/dav1d/1.5.4/dav1d-1.5.4.tar.xz
PORT=dav1d; ARCHIVES=src/libdav1d.a
configure() { meson_configure -Denable_tools=false -Denable_tests=false -Denable_examples=false -Denable_docs=false; }
make_it() { meson_make; }; install() { meson_install; }
