URL=https://downloads.sourceforge.net/project/soxr/soxr-0.1.3-Source.tar.xz
PORT=soxr; ARCHIVES=src/libsoxr.a
configure() { cmake_configure -DBUILD_EXAMPLES=OFF -DBUILD_TESTS=OFF -DWITH_OPENMP=OFF -DWITH_LSR_BINDINGS=OFF; }
make_it() { cmake_make; }; install() { cmake_install; }
