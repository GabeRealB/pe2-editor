URL=https://downloads.sourceforge.net/project/opencore-amr/vo-amrwbenc/vo-amrwbenc-0.1.3.tar.gz
PORT=vo-amrwbenc; ARCHIVES=.libs/libvo-amrwbenc.a=vo-amrwbenc
configure() { autotools_configure; }; make_it() { autotools_make; }; install() { autotools_install; }
