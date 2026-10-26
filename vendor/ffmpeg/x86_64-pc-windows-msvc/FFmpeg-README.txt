LuatOS CLI HZV minimal FFmpeg
FFmpeg 8.1.3 + static LAME 3.100
License: FFmpeg LGPL-2.1-or-later; LAME LGPL-2.0-or-later. See FFmpeg-LICENSE.txt.
Rebuild: python scripts/build_hzv_ffmpeg.py
Build options: scripts/build_hzv_ffmpeg.sh
Input: local common media containers, H.264/HEVC/MPEG-4/VP8/VP9/WMV and common audio.
Output: MJPEG, MP3 (libmp3lame), PCM S16LE only.
No network, devices, hardware acceleration, subtitles, AV1, or general-purpose video encoding.

Sources:
ffmpeg: https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz
SHA256: 7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
lame: https://downloads.sourceforge.net/project/lame/lame/3.100/lame-3.100.tar.gz
SHA256: ddfe36cab873794038ae2c1210557ad34857a4b6bdc515785d1da9e175b1da1e
nasm_windows: https://www.nasm.us/pub/nasm/releasebuilds/2.16.03/win64/nasm-2.16.03-win64.zip
SHA256: 3ee4782247bcb874378d02f7eab4e294a84d3d15f3f6ee2de2f47a46aa7226e6

Configuration:
"--prefix=/c/Users/doudou/Documents/Codex/2026-10-22/e-work-airm2m-code-airmaster-hzv-2/work/ffmpeg-minimal/build/prefix --disable-everything --disable-autodetect --disable-network --disable-avdevice --disable-hwaccels --disable-doc --disable-debug --disable-ffplay --disable-ffprobe --enable-ffmpeg --enable-small --enable-static --disable-shared --enable-libmp3lame --enable-protocol=file --enable-decoder='h264,hevc,mpeg4,mpeg1video,mpeg2video,h263,h263i,h263p,msmpeg4v1,msmpeg4v2,msmpeg4v3,wmv1,wmv2,wmv3,vc1,flv,vp6,vp6f,vp6a,vp8,vp9,mjpeg,png,bmp,gif,rawvideo,ffv1,huffyuv,utvideo,prores,dnxhd,theora,aac,aac_fixed,aac_latm,mp3,mp3float,mp2,mp2float,vorbis,opus,flac,alac,ac3,eac3,dca,wmav1,wmav2,wmapro,wmavoice,pcm_*,adpcm_*' --enable-demuxer='mov,matroska,avi,flv,mpegts,mpegtsraw,mpegvideo,mpegps,asf,aac,mp3,wav,flac,ogg,image2,image2pipe,h264,hevc,mjpeg' --enable-parser='h264,hevc,mpeg4video,mpegvideo,h263,vc1,vp8,vp9,aac,aac_latm,mpegaudio,ac3,dca,flac,opus,vorbis,mjpeg,png' --enable-encoder='mjpeg,libmp3lame,pcm_s16le' --enable-muxer='mjpeg,mp3,pcm_s16le' --enable-bsf='extract_extradata,h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc,mpeg4_unpack_bframes,null' --enable-filter='fps,scale,pad,crop,apad,aresample,aformat,format,anull,null,trim,atrim,transpose,hflip,vflip' --extra-cflags='-I/c/Users/doudou/Documents/Codex/2026-10-22/e-work-airm2m-code-airmaster-hzv-2/work/ffmpeg-minimal/build/prefix/include -ffunction-sections -fdata-sections' --extra-ldflags=-L/c/Users/doudou/Documents/Codex/2026-10-22/e-work-airm2m-code-airmaster-hzv-2/work/ffmpeg-minimal/build/prefix/lib --target-os=mingw32 --arch=x86_64 --extra-ldflags=-static"