/* luatos_log.h — C ABI for luatos-log SOC binary log decoder
 *
 * 解码 Air1601 / Air1602 / CCM4211 SOC 二进制日志帧 (0xA5 边界 + 0xA6 转义).
 * 与第三方 DLL (pySoclogAnalyze) 行为完全一致: 日志帧 (cmd==0) 与
 * 命令帧 (cmd!=0) 双模式支持. 详细文档见 docs/luatos-log-c-abi.md.
 *
 * 编译器: MSVC / gcc / clang, C99+, extern "C" 链接 (cdecl).
 * 默认调用约定: cdecl (extern "C" 默认).
 */

#ifndef LUATOS_LOG_H
#define LUATOS_LOG_H

#include <stdint.h>

#if defined(_WIN32) || defined(__CYGWIN__)
#  ifdef LUATOS_LOG_BUILDING
#    define LUATOS_API __declspec(dllexport)
#  else
#    define LUATOS_API __declspec(dllimport)
#  endif
#else
#  if defined(__GNUC__) && __GNUC__ >= 4
#    define LUATOS_API __attribute__((visibility("default")))
#  else
#    define LUATOS_API
#  endif
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* 错误码 (ABI 稳定, 新增只能追加, 不能改值) */
#define LUATOS_SOCLOG_OK             0   /* 成功 */
#define LUATOS_SOCLOG_ERR_CRC       -1   /* CRC16 校验失败 */
#define LUATOS_SOCLOG_ERR_HEADER    -2   /* header 长度不足 (< 26 字节) */
#define LUATOS_SOCLOG_ERR_BUFFER    -3   /* 输出 buffer 写不下 (保留码) */
#define LUATOS_SOCLOG_ERR_PARAM     -4   /* 任一必填指针为 NULL */
#define LUATOS_SOCLOG_ERR_EMPTY     -5   /* 输入数据为空 (len == 0) */
#define LUATOS_SOCLOG_ERR_TRUNC     -6   /* 帧超过 8192 字节被截断 */

/* 主入口 (与第三方 DLL 签名完全一致)
 *
 * @param data     [in]  原始字节流, 含 0xA5 边界与 0xA6 转义
 * @param len      [in]  data 字节数
 * @param out      [out] 日志帧: NUL 终止的 [device_time] L/module message;
 *                        命令帧: cHead.len 字节 raw payload, 截断到 64KB-1
 * @param tick     [out] 设备时间戳 (ms, u64)
 * @param sn       [out] 序列号 (u16)
 * @param tag      [out] 日志帧: 模块名 (NUL 终止, 最长 63 字节);
 *                        命令帧: 空字符串 (tag[0] = 0)
 * @param cpu      [out] CPU 编号
 * @param type     [out] msg_type 字段 (0 = printf, 其他 = raw)
 * @param cmd      [out] 命令码; 0 = 日志帧, 非 0 = 命令帧 cmd 字段
 * @param address  [out] 日志帧: 固定 0; 命令帧: 帧的 address 字段
 *
 * @return 0 成功; 负值见上方错误码
 *
 * 线程安全: 每次调用独立 (无全局状态), 多线程可并发.
 * 调用方应保证 out/tag 至少 64B 容量, 推荐 64KB; 不足时内容被截断 (不报错),
 * 末尾仍保证 NUL 终止.
 */
LUATOS_API int luatos_soclog_analyze(
    uint8_t*  data,
    uint32_t  len,
    char*     out,
    uint64_t* tick,
    uint16_t* sn,
    char*     tag,
    uint8_t*  cpu,
    uint8_t*  type,
    uint32_t* cmd,
    uint32_t* address);

/* pySoclogAnalyze 别名 (与 luatos_soclog_analyze 完全一致), 兼容第三方 DLL 调用方.
 * 现有 Python ctypes / C/C++ 代码可直接把 luatos_log_ffi 替换原 DLL, 无需修改. */
LUATOS_API int pySoclogAnalyze(
    uint8_t*  data,
    uint32_t  len,
    char*     out,
    uint64_t* tick,
    uint16_t* sn,
    char*     tag,
    uint8_t*  cpu,
    uint8_t*  type,
    uint32_t* cmd,
    uint32_t* address);

/* 查询 FFI 库版本号 (当前 ABI 1.0.0).
 * @param major [out] 主版本
 * @param minor [out] 次版本
 * @param patch [out] 修订号
 */
LUATOS_API void luatos_soclog_version(
    uint32_t* major,
    uint32_t* minor,
    uint32_t* patch);

#ifdef __cplusplus
}
#endif

#endif /* LUATOS_LOG_H */
