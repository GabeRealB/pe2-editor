URL=https://github.com/tukaani-project/xz/releases/download/v5.8.4/xz-5.8.4.tar.xz
PORT=xz; ARCHIVES=liblzma.a=lzma
configure() { cmake_configure -DXZ_TOOL_XZ=OFF -DXZ_TOOL_XZDEC=OFF -DXZ_TOOL_LZMADEC=OFF -DXZ_TOOL_LZMAINFO=OFF -DXZ_TOOL_SCRIPTS=OFF -DXZ_DOC=OFF -DXZ_NLS=OFF -DXZ_THREADS=$( [ $HOST = windows ] && echo vista || echo posix ); }
make_it() { ninja -v liblzma; }; install() { cmake --install . --prefix $SYS --component liblzma_Development; cmake --install . --prefix $SYS --component liblzma_Runtime; }
