fn main() {
    let sdk_src = "src/lzma_sdk/src";
    let sdk_inc = "src/lzma_sdk/include";

    cc::Build::new()
        .include(sdk_inc)
        .define("_7ZIP_ST", None)
        .file("src/lzma_ffi.c")
        .file(&format!("{}/LzmaLib.c", sdk_src))
        .file(&format!("{}/LzmaEnc.c", sdk_src))
        .file(&format!("{}/LzmaDec.c", sdk_src))
        .file(&format!("{}/LzFind.c", sdk_src))
        .file(&format!("{}/Alloc.c", sdk_src))
        .compile("lzma_sdk");
}
