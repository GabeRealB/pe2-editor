# allyourcodebase's port, which FFmpeg's port links, without iconv as it builds it (macOS has iconv in its SDK alone).
# Its headers where its .pc file names them, which FFmpeg's configure asks for.
allyourcodebase_installed() {
  rm -rf "$SYS/include/libxml2"; mkdir -p "$SYS/include/libxml2"; mv "$SYS/include/libxml" "$SYS/include/libxml2/"
  sys_pc libxml-2.0 2.15.4 -lxml2 "$( [ $HOST = windows ] && echo '-lws2_32 -lbcrypt' || echo -lm )" "-I\${includedir}/libxml2 -DLIBXML_STATIC"
}
run() { allyourcodebase libxml2 xml xml2 -Diconv=false; }
