URL=https://github.com/uclouvain/openjpeg/archive/refs/tags/v2.5.4.tar.gz
PORT=openjpeg; ARCHIVES=bin/libopenjp2.a=openjp2
configure() { cmake_configure -DBUILD_CODEC=OFF -DBUILD_TESTING=OFF -DBUILD_JPIP=OFF -DBUILD_DOC=OFF; }
make_it() { cmake_make; }; install() { cmake_install; }
