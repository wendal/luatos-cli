/* demo.c — luatos-log-ffi C 调用示例
 *
 * 演示如何调用 luatos_soclog_analyze / pySoclogAnalyze 解码 SOC 日志帧.
 * 帧数据使用一个预构造的示例 (hex 字符串), 避免在示例代码里重复 256 项
 * CRC 表. 真实场景下, 帧数据应从串口读取.
 *
 * 编译:
 *   Linux/macOS: 见同目录 Makefile (make && make test)
 *   Windows MSVC: 见同目录 build.bat
 *
 * 运行前确保 luatos_log_ffi.{so,dll,dylib} 在库搜索路径中:
 *   Linux:   LD_LIBRARY_PATH=target/release ./demo
 *   macOS:   DYLD_LIBRARY_PATH=target/release ./demo
 *   Windows: 把 dll 放到 PATH 或同目录
 */

#include "luatos_log.h"
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* hex 字符串转 bytes, 返回写入 dst 的字节数 */
static int hex_to_bytes(const char *hex, uint8_t *dst, int dst_cap) {
    int n = 0;
    while (*hex && n < dst_cap) {
        char c1 = hex[0], c2 = hex[1];
        if (c1 == 0 || c2 == 0) break;
        int v = 0;
        if (c1 >= '0' && c1 <= '9') v = (c1 - '0') << 4;
        else if (c1 >= 'A' && c1 <= 'F') v = (c1 - 'A' + 10) << 4;
        else if (c1 >= 'a' && c1 <= 'f') v = (c1 - 'a' + 10) << 4;
        else return -1;
        if (c2 >= '0' && c2 <= '9') v |= (c2 - '0');
        else if (c2 >= 'A' && c2 <= 'F') v |= (c2 - 'A' + 10);
        else if (c2 >= 'a' && c2 <= 'f') v |= (c2 - 'a' + 10);
        else return -1;
        dst[n++] = (uint8_t)v;
        hex += 2;
    }
    return n;
}

int main(void) {
    uint32_t major = 0, minor = 0, patch = 0;
    luatos_soclog_version(&major, &minor, &patch);
    printf("luatos-log-ffi version %u.%u.%u\n", major, minor, patch);

    /* 预构造的日志帧 (cmd=0, ms=5000, level=Info, sn=42, body="Hello")
     *
     * 帧结构:
     *   0xA5                                     # 帧头
     *   88 13 00 00 00 00 00 00                  # ms = 0x1388 = 5000 (LE)
     *   02 00 00 00 00 00 00 00                  # tag = level 2 (Info)
     *   00 00 00 00                              # cmd = 0 (log frame)
     *   2A 00                                    # sn = 42
     *   00 00                                    # type=0, cpu=0
     *   48 65 6C 6C 6F 00 00 00                  # body: "Hello\0" + pad
     *   <CRC16 LE>                               # 校验
     *   0xA5                                     # 帧尾
     */
    const char *frame_hex =
        "A5" "8813000000000000" "0200000000000000" "00000000" "2A00" "0000"
        "48656C6C6F000000" "6844" "A5";

    uint8_t frame[256];
    int frame_len = hex_to_bytes(frame_hex, frame, sizeof(frame));
    if (frame_len <= 0) {
        printf("hex parse failed\n");
        return 1;
    }
    printf("Built %d byte frame\n", frame_len);

    /* 调用主入口 */
    char out[64 * 1024] = {0};
    char tag[64] = {0};
    uint64_t tick = 0;
    uint16_t sn = 0;
    uint8_t cpu = 0, type = 0;
    uint32_t cmd = 0, address = 0;

    int rc = luatos_soclog_analyze(
        frame, (uint32_t)frame_len,
        out, &tick, &sn, tag,
        &cpu, &type, &cmd, &address);

    if (rc == LUATOS_SOCLOG_OK) {
        printf("OK: tick=%llu sn=%u cpu=%u type=%u cmd=%u address=0x%08X\n",
               (unsigned long long)tick, sn, cpu, type, cmd, address);
        printf("    tag='%s'\n", tag);
        printf("    out='%s'\n", out);
    } else {
        printf("luatos_soclog_analyze FAILED: rc=%d\n", rc);
        return 1;
    }

    /* 验证别名等价 */
    memset(out, 0, sizeof(out));
    rc = pySoclogAnalyze(
        frame, (uint32_t)frame_len,
        out, &tick, &sn, tag,
        &cpu, &type, &cmd, &address);
    if (rc == LUATOS_SOCLOG_OK) {
        printf("pySoclogAnalyze alias works: out='%s'\n", out);
    } else {
        printf("pySoclogAnalyze FAILED: rc=%d\n", rc);
        return 1;
    }

    /* 错误处理示例 */
    rc = luatos_soclog_analyze(NULL, 0, out, &tick, &sn, tag, &cpu, &type, &cmd, &address);
    printf("NULL param -> rc=%d (expected -4 LUATOS_SOCLOG_ERR_PARAM)\n", rc);

    return 0;
}
