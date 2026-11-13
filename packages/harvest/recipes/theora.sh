URL=https://downloads.xiph.org/releases/theora/libtheora-1.2.0.tar.xz
PORT=theora; ARCHIVES="lib/.libs/libtheoradec.a=theoradec lib/.libs/libtheoraenc.a=theoraenc"
configure() { autotools_configure --with-ogg=$SYS $( [ $HOST = windows ] && echo --disable-asm ) --disable-examples --disable-doc --disable-spec --disable-oggtest --disable-vorbistest --disable-sdltest; }
make_it() { autotools_make; }; install() { autotools_install; }
# Windows: without libtheora's x86 assembly, inline assembly written for the System V calling convention, which crashes
# encoding on 64-bit Windows.
