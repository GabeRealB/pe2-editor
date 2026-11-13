URL=https://downloads.sourceforge.net/project/freetype/freetype2/2.14.3/freetype-2.14.3.tar.xz
PORT=freetype; ARCHIVES=libfreetype.a=freetype
# CMake's build: meson's wants a Windows resource compiler even for a static library.
# On Windows it compiles a DLL's version resource even into a static library: left out, there being no DLL.
configure() { sed -i -e '/^  enable_language(RC)$/d' -e 's|^                        src/base/ftver.rc)$|)|' $SRC/CMakeLists.txt && cmake_configure -DFT_DISABLE_ZLIB=ON -DFT_DISABLE_BZIP2=ON -DFT_DISABLE_PNG=ON -DFT_DISABLE_HARFBUZZ=ON -DFT_DISABLE_BROTLI=ON; }
make_it() { cmake_make; }; install() { cmake_install; }
