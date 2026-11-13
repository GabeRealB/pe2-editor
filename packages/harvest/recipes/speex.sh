URL=https://downloads.xiph.org/releases/speex/speex-1.2.1.tar.gz
PORT=speex; ARCHIVES=libspeex/.libs/libspeex.a=speex
configure() { autotools_configure --disable-binaries; }
make_it() { autotools_make; }; install() { autotools_install; }
