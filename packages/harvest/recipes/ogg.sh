URL=https://downloads.xiph.org/releases/ogg/libogg-1.3.6.tar.xz
PORT=ogg
ARCHIVES=libogg.a
configure() { cmake -G Ninja $SRC -DCMAKE_TOOLCHAIN_FILE=$TOOLCHAIN -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$SYS -DBUILD_SHARED_LIBS=OFF -DINSTALL_DOCS=OFF; }
make_it() { ninja -v; }
install() { cmake --install . --prefix $SYS; }
