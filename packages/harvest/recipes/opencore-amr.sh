URL=https://downloads.sourceforge.net/project/opencore-amr/opencore-amr/opencore-amr-0.1.6.tar.gz
PORT=opencore-amr; ARCHIVES="amrnb/.libs/libopencore-amrnb.a=opencore-amrnb amrwb/.libs/libopencore-amrwb.a=opencore-amrwb"
configure() { autotools_configure --disable-examples; }; make_it() { autotools_make; }; install() { autotools_install; }
