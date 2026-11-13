URL=https://github.com/mm2/Little-CMS/releases/download/lcms2.20/lcms2-2.20.tar.gz
PORT=lcms2; ARCHIVES=src/.libs/liblcms2.a=lcms2
configure() { autotools_configure --without-jpeg --without-tiff --without-zlib; }
make_it() { make -j$JOBS V=1 -C src; }; install() { make -C src install && make -C include install && make install-pkgconfigDATA; }
