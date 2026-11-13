URL=https://dri.freedesktop.org/libdrm/libdrm-2.4.134.tar.xz
# What Linux's graphics drivers are reached through: Linux's alone.
HOSTS=linux
PORT=libdrm; ARCHIVES=libdrm.a=drm
configure() { meson_configure -Dintel=disabled -Dradeon=disabled -Damdgpu=disabled -Dnouveau=disabled -Dvmwgfx=disabled -Domap=disabled -Dexynos=disabled -Dfreedreno=disabled -Dtegra=disabled -Dvc4=disabled -Detnaviv=disabled -Dcairo-tests=disabled -Dman-pages=disabled -Dvalgrind=disabled -Dtests=false -Dinstall-test-programs=false -Dudev=false; }
make_it() { meson_make; }; install() { meson_install; }
