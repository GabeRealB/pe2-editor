/* Copyright (c) 2011 The WebM project authors. All Rights Reserved. */
/*  */
/* Use of this source code is governed by a BSD-style license */
/* that can be found in the LICENSE file in the root of the source */
/* tree. An additional intellectual property rights grant can be found */
/* in the file PATENTS.  All contributing project authors may */
/* be found in the AUTHORS file in the root of the source tree. */
#include "vpx/vpx_codec.h"
static const char* const cfg = "--target=arm64-darwin20-gcc --prefix=/usr/local --as=nasm --disable-examples --disable-tools --disable-docs --disable-unit-tests --enable-vp9-highbitdepth --enable-runtime-cpu-detect --enable-pic --disable-shared --enable-static";
const char *vpx_codec_build_config(void) {return cfg;}
