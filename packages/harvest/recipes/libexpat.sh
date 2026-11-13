# allyourcodebase's port, which fontconfig's port links, for fontconfig's build to find.
allyourcodebase_installed() { sys_pc expat 2.9.0 -lexpat "" -DXML_STATIC; }
run() { allyourcodebase libexpat expat expat; }
