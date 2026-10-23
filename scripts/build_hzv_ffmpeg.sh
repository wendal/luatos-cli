#!/usr/bin/env bash
# 仅构建 HZV 所需的 FFmpeg，源码由 build_hzv_ffmpeg.py 按 SHA256 准备。
set -euo pipefail

# 非登录 MSYS shell 不读取 profile，先补齐基础构建工具路径。
case "${OSTYPE:-}" in msys*|cygwin*) export PATH="/ucrt64/bin:/usr/bin:$PATH" ;; esac

ffmpeg_source=$(cd "$1" && pwd)
lame_source=$(cd "$2" && pwd)
mkdir -p "$3" "$4"
build_root=$(cd "$3" && pwd)
destination=$(cd "$4" && pwd)
jobs=${HZV_BUILD_JOBS:-8}
prefix="$build_root/prefix"
host_options=()
platform_options=()
link_options=()
make_tool=make
case "$(uname -s)" in
    MINGW*|MSYS*)
        export PATH="/ucrt64/bin:/usr/bin:$PATH"
        host_options=(--host=x86_64-w64-mingw32)
        platform_options=(--target-os=mingw32 --arch=x86_64)
        link_options=(--extra-ldflags=-static)
        # MSYS make 识别 /c/... 路径；UCRT 原生 make 会把它误认为不存在。
        make_tool=/usr/bin/make
        ;;
esac

mkdir -p "$build_root/lame" "$build_root/ffmpeg" "$destination/bin"
printf '构建静态 LAME\n'
cd "$build_root/lame"
if [[ ! -f config.status || ${HZV_RECONFIGURE:-0} == 1 ]]; then
if ! CFLAGS='-Os -ffunction-sections -fdata-sections' bash "$lame_source/configure" \
    --prefix="$prefix" --disable-shared --enable-static --disable-frontend \
    --disable-decoder --disable-gtktest --disable-dependency-tracking \
    "${host_options[@]}" > configure.log 2>&1; then
    tail -80 configure.log
    exit 1
fi
fi
if ! "$make_tool" -j"$jobs" > build.log 2>&1; then tail -80 build.log; exit 1; fi
"$make_tool" install > install.log 2>&1

printf '配置精简 FFmpeg\n'
cd "$build_root/ffmpeg"
# 本地常见视频输入；不加入外部 AV1 编解码、字幕、网络或硬件加速库。
decoders='h264,hevc,mpeg4,mpeg1video,mpeg2video,h263,h263i,h263p,msmpeg4v1,msmpeg4v2,msmpeg4v3,wmv1,wmv2,wmv3,vc1,flv,vp6,vp6f,vp6a,vp8,vp9,mjpeg,png,bmp,gif,rawvideo,ffv1,huffyuv,utvideo,prores,dnxhd,theora,aac,aac_fixed,aac_latm,mp3,mp3float,mp2,mp2float,vorbis,opus,flac,alac,ac3,eac3,dca,wmav1,wmav2,wmapro,wmavoice,pcm_*,adpcm_*'
demuxers='mov,matroska,avi,flv,mpegts,mpegtsraw,mpegvideo,mpegps,asf,aac,mp3,wav,flac,ogg,image2,image2pipe,h264,hevc,mjpeg'
filters='fps,scale,pad,crop,apad,aresample,aformat,format,anull,null,trim,atrim,transpose,hflip,vflip'
if [[ ! -f ffbuild/config.mak || ${HZV_RECONFIGURE:-0} == 1 ]]; then
if ! bash "$ffmpeg_source/configure" \
    --prefix="$prefix" --disable-everything --disable-autodetect \
    --disable-network --disable-avdevice --disable-hwaccels \
    --disable-doc --disable-debug --disable-ffplay --disable-ffprobe \
    --enable-ffmpeg --enable-small --enable-static --disable-shared \
    --enable-libmp3lame --enable-protocol=file \
    --enable-decoder="$decoders" --enable-demuxer="$demuxers" \
    --enable-parser='h264,hevc,mpeg4video,mpegvideo,h263,vc1,vp8,vp9,aac,aac_latm,mpegaudio,ac3,dca,flac,opus,vorbis,mjpeg,png' \
    --enable-encoder=mjpeg,libmp3lame,pcm_s16le \
    --enable-muxer=mjpeg,mp3,pcm_s16le \
    --enable-bsf=extract_extradata,h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc,mpeg4_unpack_bframes,null \
    --enable-filter="$filters" \
    --extra-cflags="-I$prefix/include -ffunction-sections -fdata-sections" \
    --extra-ldflags="-L$prefix/lib" \
    "${platform_options[@]}" "${link_options[@]}" > configure.log 2>&1; then
    tail -100 configure.log
    exit 1
fi
fi
printf '编译精简 FFmpeg（%s 个并行任务）\n' "$jobs"
if ! "$make_tool" -j"$jobs" > build.log 2>&1; then tail -100 build.log; exit 1; fi
binary=ffmpeg
if [[ -f ffmpeg.exe ]]; then binary=ffmpeg.exe; fi
cp "$binary" "$destination/bin/$binary"
strip "$destination/bin/$binary"
cp config.h "$destination/build-config.h"
cp config_components.h "$destination/build-components.h"
cp config.log "$destination/config.log" 2>/dev/null || cp ffbuild/config.log "$destination/config.log"
printf '精简 FFmpeg 构建完成：%s\n' "$destination/bin/$binary"
