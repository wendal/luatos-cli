# romfs 测试资产

`romfs_fixture.img` 由 LuatOS 核心的 `tools/romfs/mkromfs.py` 生成, 用于校验
`src/romfs.rs` 的编码器与 canonical 实现**逐字节一致**。

输入目录内容(全部为文本, 无换行差异):

| 相对路径 | 内容 |
| --- | --- |
| `hello.txt` | `hello romfs\n` |
| `eighty.txt` | `0123456789` 重复 7 次(70 字节) |
| `sub/a.bin` | `AAAA` |
| `sub/b.bin` | `BBBBBBBB` |
| `sub/deep/c.dat` | `xyz` |

重新生成:

```bash
python tools/romfs/mkromfs.py <上面的目录> -o crates/luatos-luadb/tests/data/romfs_fixture.img
python tools/romfs/mkromfs.py crates/luatos-luadb/tests/data/romfs_fixture.img --verify
```

`--verify` 应打印 432 字节与完整的目录树。
