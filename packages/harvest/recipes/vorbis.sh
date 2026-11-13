URL=https://downloads.xiph.org/releases/vorbis/libvorbis-1.3.7.tar.xz
PORT=vorbis
ARCHIVES="lib/libvorbis.a lib/libvorbisenc.a"
configure() { cmake -G Ninja $SRC -DCMAKE_TOOLCHAIN_FILE=$TOOLCHAIN -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$SYS -DBUILD_SHARED_LIBS=OFF -DCMAKE_POLICY_VERSION_MINIMUM=3.5; }
make_it() { ninja -v; }
install() { cmake --install . --prefix $SYS; }
