# Air8101(BK72XX) FOTA 包格式说明（新格式）

本文档基于 `D:\github\luatools_py3\bk72xx_fota.py` 当前实现整理，用于 `luatos-cli` 内 Air8101/BK72XX FOTA 生成逻辑落地。

## 1. 适用范围

- 芯片：`air8101` / `bk72xx`
- 前提：`info.json` 中 `rom.fs.script.bkcrc=true`
- 本实现仅覆盖**新格式**：
  - 全量 FOTA（RBL 包）
  - 脚本 FOTA（1KB 头 + CRC16 脚本区）

## 2. 脚本 FOTA（script-only）格式

二进制布局：

1. `0x0000..0x03FF`：1024 字节 `0xFF` 填充
2. `0x0400..0x0403`：魔数 `0x4C554154`（LE，"Luat"）
3. `0x0404..0x0407`：payload 长度（LE）
4. `0x0408..`：脚本 payload

脚本 payload 生成规则：

1. 读取 `script.bin`，去掉尾部 `0xFF`
2. 按 32 字节对齐补 `0xFF`
3. 每 32 字节附加 2 字节 CRC16（BK 算法，BE）
4. 最终再按 4 字节对齐补 `0xFF`

## 3. 全量 FOTA（RBL）格式

### 3.1 原始载荷拼装

1. 读取 `cp.bin` / `ap.bin` / `script.bin`
2. 计算脚本在 AP 内偏移：
   - `script_offset_flash = script_addr - ap_offset`
   - `script_offset_rbl_raw = script_offset_flash * 32 / 34`
   - `ap_script_offset = ceil(script_offset_rbl_raw, 32)`
3. `ap.bin` 末尾先追加 8 字节 `0xFF`，再填充到 `ap_script_offset`
4. `script.bin` 去尾 `0xFF` 后按 32 字节对齐，追加到 AP 尾部
5. `cp.bin` 若不足 `0xF0000`，补齐到 `0xF0000`
6. 组合 `raw_payload = cp_padded + ap_with_script`

### 3.2 压缩与加密

1. `gzip(level=9)` 压缩 `raw_payload`
2. 替换 gzip 前 10 字节为固定头：
   - `1F 8B 08 00 00 00 00 00 02 00`
3. 使用 AES-256-CBC + PKCS7 加密：
   - key: `"0123456789ABCDEF0123456789ABCDEF"`
   - iv: `"0123456789ABCDEF"`

### 3.3 RBL 头（96 字节）

- 前 92 字节字段：
  - `type_4`: `"RBL\0"`
  - `algo`: `258`（LE，gzip + aes256）
  - `ctime`: 6 字节 0
  - `app_part_name`: `"app"`（16 字节补零）
  - `download_version`: `"2"`（24 字节补零）
  - `current_version`: `"00010203040506070809"`（24 字节补零）
  - `body_crc32`: 加密后数据 CRC32（LE）
  - `hash_val`: 原始载荷 FNV1a（LE）
  - `raw_size`: 原始载荷长度（LE）
  - `com_size`: 加密后长度（LE）
- 第 93~96 字节：前 92 字节 CRC32（LE）
- 数据区：紧随其后的加密载荷

## 4. luatos-cli 当前实现约束

- 不支持 BK72XX 旧格式（`LFTA`）包输出。
- 若 `bkcrc` 不是 `true`，命令直接报错并提示仅支持新格式。
