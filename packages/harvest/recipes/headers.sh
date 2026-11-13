# What FFmpeg reaches graphics cards with, which it loads when it runs: their headers alone, as FFmpeg's port takes
# them from their releases (NVIDIA's, AMD's, Vulkan's).
run() {
  local src=$WORK/src
  unpack "$(url_of ffmpeg-9.0.2 nv_codec_headers)" $src/nv-codec-headers
  unpack "$(url_of ffmpeg-9.0.2 amf_headers)" $src/amf-headers
  unpack "$(url_of ffmpeg-9.0.2 vulkan_headers)" $src/vulkan-headers
  for host in $HOSTS; do
    . "$HERE/env.sh" $host
    make -C $src/nv-codec-headers install PREFIX=$SYS > /dev/null
    rm -rf $SYS/include/AMF; cp -r $src/amf-headers/AMF $SYS/include/
    cp -r $src/vulkan-headers/include/vulkan $src/vulkan-headers/include/vk_video $SYS/include/
  done
}
