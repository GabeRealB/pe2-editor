URL=https://github.com/libass/libass/releases/download/0.17.5/libass-0.17.5.tar.xz
PORT=libass; ARCHIVES=libass/libass.a=ass
configure() { meson_configure -Dfontconfig=enabled -Ddirectwrite=$( [ $HOST = windows ] && echo enabled || echo disabled ) -Dcoretext=disabled -Dasm=enabled -Dtest=disabled -Dcompare=disabled -Dprofile=disabled -Dfuzz=disabled -Dcheckasm=disabled -Dlibunibreak=disabled; }
make_it() { meson_make; }; install() { meson_install; }
