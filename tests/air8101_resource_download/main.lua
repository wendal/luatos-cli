PROJECT = "cli_res_hw"
VERSION = "2.0.0"
sys = require "sys"
local manifest = require "manifest"
local sentinel = "resource-download-lfs-sentinel-v1"
local result, detail = "CLI_RES_WAIT", ""
local profiles = {["full\n"] = "full", ["generic\n"] = "generic", ["deep\n"] = "deep"}
local function contents(path)
    local f = io.open(path, "rb")
    if not f then return nil end
    local data = f:read("*a")
    f:close()
    return data
end
local function verify(path, length, step, seed, reference)
    log.info("CLI_RES", "VERIFY", path, length)
    local f = assert(io.open(path, "rb"), path)
    assert(f:seek("end") == length, "size " .. path)
    assert(f:seek("set", length - 64))
    local tail = assert(f:read(64))
    if not reference then
        for i = 1, #tail do assert(tail:byte(i) == ((length - 64 + i - 1) * step + seed) % 256) end
    end
    assert(f:seek("set", 0))
    local cycle = {}
    if not reference then
        for i = 0, 255 do cycle[#cycle + 1] = (i * step + seed) % 256 end
    end
    local expected = not reference and string.char(table.unpack(cycle)):rep(16) or nil
    local script = assert(io.open(reference or "/luadb/main.luac", "rb"))
    local offset = 0
    while offset < length do
        local data = assert(f:read(math.min(4096, length - offset)))
        assert(#data > 0)
        if reference then
            assert(data == script:read(#data), "reference mismatch " .. path)
        else
            assert(data == expected:sub(1, #data), "pattern mismatch " .. path)
            assert(script:seek("set", 0))
            assert(script:read(4) == "\27Lua", "interleave")
        end
        offset = offset + #data
        if offset % 262144 == 0 then sys.wait(1) end
    end
    assert(f:read(1) == nil, "EOF")
    f:close(); script:close()
end
local function reject_write(path)
    for _, mode in ipairs({"w", "wb", "a", "r+", "r+b"}) do
        assert(io.open(path, mode) == nil, "write allowed " .. mode)
    end
end
-- /res 是帧化 romfs 只读镜像: io.fsstat 的 total*block 是镜像头部声明的长度按 512 字节向上取整,
-- 只读挂载下 used == total。
local function check_res_stat(image_size)
    local valid, total, used, block, kind = io.fsstat("/res/")
    assert(valid, "res fsstat")
    assert(kind == "romfs", "res kind " .. tostring(kind))
    assert(used == total, "res used")
    assert(total * block == math.floor((image_size + block - 1) / block) * block, "res capacity")
end
sys.taskInit(function()
    sys.wait(1000)
    local ok, err = pcall(function()
        local profile = contents("/res/profile.txt")
        if not profile then
            local f = assert(io.open("/cli_res_sentinel", "wb"))
            assert(f:write(sentinel)); f:close()
            result = "CLI_RES_SETUP_PASS"
            detail = "LFS sentinel prepared before resource download"
            return
        end
        local name = assert(profiles[profile], "unknown profile")
        assert(contents("/cli_res_sentinel") == sentinel, "LFS sentinel lost")
        local count = tonumber(contents("/cli_res_boot")) or 0
        local f = assert(io.open("/cli_res_boot", "wb")); f:write(tostring(count + 1)); f:close()
        log.info("CLI_RES", "BOOT", count + 1, name)
        for _, stat in ipairs({{"/", 1048576}, {"/luadb/", 524288}}) do
            local valid, total, used, block = io.fsstat(stat[1])
            assert(valid and total * block == stat[2], "capacity " .. stat[1])
        end
        if name == "deep" then
            check_res_stat(manifest.deep_image)
            local valid, entries = io.lsdir("/res/", 20, 0)
            assert(valid and #entries == 2, "deep root count")
            local sizes = {}
            for _, entry in ipairs(entries) do sizes[entry.name] = entry.size end
            assert(sizes["profile.txt"] == 5 and sizes["a"] == 0, "deep root sizes")
            assert(contents("/res/a/b/c/d/keep.bin") == "4-level-kept\n", "4-level file kept")
            assert(contents("/res/a/b/c/d/e/x.bin") == nil, "5th-level file skipped")
            assert(contents("/res/asset.dat") == nil, "old resource files invisible")
            local keep_valid, keep = io.lsdir("/res/a/b/c/d/", 20, 0)
            assert(keep_valid and #keep == 1 and keep[1].name == "keep.bin", "4-level dir listing")
            assert(keep[1].type == io.FILE and keep[1].size == manifest.deep_keep, "keep.bin size")
            local deeper_valid = io.lsdir("/res/a/b/c/d/e/", 20, 0)
            assert(not deeper_valid, "skipped 5th-level dir exists")
            reject_write("/res/a/b/c/d/keep.bin")
            assert(contents("/cli_res_sentinel") == sentinel)
            result = "CLI_RES_DEEP_PASS"
            detail = "4-level kept, 5th-level skipped, romfs readonly, LFS preserved"
            return
        end
        local full = name == "full"
        check_res_stat(full and manifest.full_image or manifest.generic_image)
        assert(contents("/res/note.txt") == "arbitrary resource\n")
        local valid, entries = io.lsdir("/res/", 20, 0)
        assert(valid and #entries == (full and 8 or 5), "directory count")
        local sizes = {}
        for _, entry in ipairs(entries) do sizes[entry.name] = entry.size end
        assert(sizes["note.txt"] == 19 and sizes["palette.bin"] == 80000)
        local length = full and manifest.full_asset or manifest.generic_asset
        assert(sizes["asset.dat"] == length)
        verify("/res/asset.dat", length, 37, 11)
        verify("/res/palette.bin", 80000, 13, 7)
        local boundary = string.rep("x", 64)
        if full then
            assert(contents("/res/source.lua") == "this is deliberately not valid Lua!\n\0")
            assert(contents("/res/" .. boundary) == "64-byte-name\n")
            assert(sizes[boundary] == 13)
            assert(sizes["sample.ttf"] == manifest.font_size)
            verify("/res/sample.ttf", manifest.font_size, nil, nil, "/luadb/reference.ttf")
            verify("/res/sub/pixels.bin", 3000, 5, 3)
        else
            assert(contents("/res/sample.ttf") == nil and contents("/res/source.lua") == nil)
            assert(contents("/res/" .. boundary) == nil)
            assert(contents("/res/sub/pixels.bin") == nil)
            verify("/res/new/image.bin", 3000, 5, 3)
        end
        reject_write("/res/note.txt")
        assert(contents("/cli_res_sentinel") == sentinel)
        result = full and "CLI_RES_FULL_PASS" or "CLI_RES_GENERIC_PASS"
        detail = "exact contents, seek, EOF, dir, romfs stats, interleave, readonly, LFS preserved"
    end)
    if not ok then result, detail = "CLI_RES_FAIL", tostring(err) end
    while true do log.info("CLI_RES", result, detail); sys.wait(3000) end
end)
sys.run()
