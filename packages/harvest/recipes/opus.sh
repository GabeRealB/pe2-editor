URL=https://downloads.xiph.org/releases/opus/opus-1.6.1.tar.gz
PORT=opus; ARCHIVES=libopus.a
configure() { cmake_configure -DOPUS_BUILD_PROGRAMS=OFF -DOPUS_BUILD_TESTING=OFF; }
make_it() { cmake_make; }; install() { cmake_install; }
