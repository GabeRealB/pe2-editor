URL=https://github.com/intel/libva/archive/refs/tags/2.24.1.tar.gz
# What Linux's graphics drivers are reached through: Linux's alone.
HOSTS=linux
PORT=libva; ARCHIVES="va/libva.a=va va/libva-drm.a=va-drm"
# Its configuration where systems keep it (/etc/libva.conf); the drivers it loads, where distributions keep them
# (Debian's and Ubuntu's, Fedora's and Arch's), as LIBVA_DRIVERS_PATH can say otherwise.
# Its build makes shared libraries only: libraries as the build is asked for (static), as static builds of it do.
configure() { sed -i 's/= shared_library(/= library(/' $SRC/va/meson.build && meson_configure -Dsysconfdir=/etc -Ddriverdir=/usr/lib/x86_64-linux-gnu/dri:/usr/lib64/dri:/usr/lib/dri:/usr/local/lib/dri -Dwith_x11=no -Dwith_glx=no -Dwith_wayland=no -Dwith_win32=no -Denable_docs=false; }
make_it() { meson_make; }; install() { meson_install; }
