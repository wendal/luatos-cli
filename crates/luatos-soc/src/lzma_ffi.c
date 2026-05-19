// lzma_ffi.c — Thin FFI wrapper around LZMA SDK's LzmaCompress.

#include "LzmaLib.h"

// LzmaCompress is declared in LzmaEnc.h, defined in LzmaEnc.c.
// This file just provides a re-export so Rust can link against it.
// The build.rs compiles LzmaEnc.c, LzFind.c, Alloc.c alongside this file.

// Re-export: provide a wrapper with simpler unsigned types for FFI
int lzma_sdk_compress(
    const unsigned char *src, unsigned int src_len,
    unsigned char *dest, unsigned int *dest_len,
    unsigned char *props, unsigned int *props_size,
    int level, unsigned int dict_size,
    int lc, int lp, int pb, int fb
) {
    size_t destLen = (size_t)*dest_len;
    size_t propsSize = (size_t)*props_size;
    int result = LzmaCompress(dest, &destLen, src, (size_t)src_len,
                               props, &propsSize,
                               level, (unsigned)dict_size,
                               lc, lp, pb, fb, 1);
    *dest_len = (unsigned int)destLen;
    *props_size = (unsigned int)propsSize;
    return result;
}
