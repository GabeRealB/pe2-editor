# FFmpeg, configured against every library harvested before it and made, for each host; its port's host files are
# written from what configure wrote and from what make ran (`ffmpeg.py`), as its makefiles' commands cannot be logged
# apart from FFmpeg's own steps.
PORT=ffmpeg-9.0.2

# Every library of the media set, and resources left uncompressed (compressing them asks for gzip on the machine).
OPTIONS="--disable-autodetect --enable-gpl --enable-version3
  --disable-resource-compression --enable-libx264 --enable-libvorbis --enable-libopus
  --enable-libspeex --enable-libtheora --enable-lzma --enable-libopenjpeg --enable-libwebp
  --enable-libmp3lame --enable-bzlib --enable-zlib --enable-libxml2 --enable-libgsm
  --enable-libopencore-amrnb --enable-libopencore-amrwb --enable-libvo-amrwbenc --enable-libsoxr
  --enable-libxvid --enable-libdav1d --enable-libaom --enable-libsvtav1 --enable-libvpx
  --enable-libopenh264 --enable-libx265 --enable-libjxl --enable-libzimg --enable-libass
  --enable-libfreetype --enable-libharfbuzz --enable-libfribidi --enable-libfontconfig"
# What reaches graphics cards, by host: NVIDIA's, AMD's and Vulkan's on Linux and Windows, VA-API on Linux, Direct3D
# and Media Foundation on Windows; on macOS VideoToolbox would, whose headers are Apple's SDK's.
HARDWARE_linux="--enable-ffnvcodec --enable-nvenc --enable-nvdec --enable-cuvid --enable-amf --enable-vulkan --enable-vaapi --enable-libdrm"
HARDWARE_windows="--enable-ffnvcodec --enable-nvenc --enable-nvdec --enable-cuvid --enable-amf --enable-vulkan --enable-d3d11va --enable-dxva2 --enable-d3d12va --enable-mediafoundation"
HARDWARE_macos=""

run() {
  local src=$WORK/src/ffmpeg
  unpack "$(url_of $PORT ffmpeg)" "$src"
  for host in $HOSTS; do
    . "$HERE/env.sh" $host
    local b=$WORK/build/ffmpeg-$host os arch exe="" hardware=HARDWARE_$host
    case $host in linux) os=linux; arch=x86_64 ;; macos) os=darwin; arch=aarch64 ;; windows) os=mingw64; arch=x86_64; exe=.exe ;; esac
    rm -rf "$b"; mkdir -p "$b"; cd "$b"
    "$src/configure" --enable-cross-compile --target-os=$os --arch=$arch --cc=$CC --cxx=$CXX --ar=$AR --ranlib=$RANLIB \
      $( [ $host = macos ] && echo --nm=$NM ) $( [ $arch = x86_64 ] && echo --x86asmexe=$TOOLS/bin/nasm ) \
      --pkg-config=pkg-config --pkg-config-flags=--static --extra-cflags=-I$SYS/include --extra-ldflags=-L$SYS/lib \
      --disable-doc --disable-debug --disable-stripping --enable-static --disable-shared $OPTIONS ${!hardware} \
      > configure.log 2>&1 || { echo "ffmpeg $host: configure failed (see $b/ffbuild/config.log)"; tail -3 configure.log; exit 1; }
    # zig's glibc headers are current ones, which no longer have sys/sysctl.h; glibc 2.28's sysctl() links all the same.
    [ $host = linux ] && sed -i 's/HAVE_SYSCTL 1/HAVE_SYSCTL 0/' config.h config.asm
    # -O: each job's output together, which the analysis reads (a parallel job's warnings would split a command).
    make -j$JOBS -O V=1 ffmpeg$exe ffprobe$exe > make.log 2>&1 || { echo "ffmpeg $host: make failed (see $b/make.log)"; exit 1; }
  done
  python3 "$HERE/ffmpeg.py" "$WORK" "$src" "$PACKAGES/ports/$PORT" $HOSTS
}
