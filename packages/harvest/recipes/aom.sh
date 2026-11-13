URL=git+https://aomedia.googlesource.com/aom#v3.15.3
PORT=aom; ARCHIVES=libaom.a
configure() { cmake_configure -DENABLE_DOCS=0 -DENABLE_EXAMPLES=0 -DENABLE_TESTS=0 -DENABLE_TOOLS=0 -DENABLE_TESTDATA=0 -DCONFIG_RUNTIME_CPU_DETECT=1 -DAOM_TARGET_CPU=$( [ $HOST = macos ] && echo arm64 || echo x86_64 ); }
make_it() { cmake_make; }; install() { cmake_install; }
