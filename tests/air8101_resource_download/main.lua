PROJECT = "cli_res_hw"
VERSION = "1.0.0"
sys = require "sys"
local manifest = require "manifest"
local sentinel = "resource-download-lfs-sentinel-v1"
local result, detail = "CLI_RES_WAIT", ""
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
        assert(profile == "full\n" or profile == "generic\n", "unknown profile")
        local full = profile == "full\n"
        assert(contents("/cli_res_sentinel") == sentinel, "LFS sentinel lost")
        local count = tonumber(contents("/cli_res_boot")) or 0
        local f = assert(io.open("/cli_res_boot", "wb")); f:write(tostring(count + 1)); f:close()
        log.info("CLI_RES", "BOOT", count + 1, full and "full" or "generic")
        for _, stat in ipairs({{"/", 1048576}, {"/luadb/", 524288}, {"/res/", 3145728}}) do
            local valid, total, used, block, kind = io.fsstat(stat[1])
            assert(valid and total * block == stat[2], "capacity " .. stat[1])
            if stat[1] == "/res/" then assert(used == total and kind == "luadb") end
        end
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
        if full then
            assert(contents("/res/source.lua") == "this is deliberately not valid Lua!\n\0")
            assert(contents("/res/" .. string.rep("x", 31)) == "31-byte-name\n")
            assert(sizes["sample.ttf"] == manifest.font_size)
            verify("/res/sample.ttf", manifest.font_size, nil, nil, "/luadb/reference.ttf")
            verify("/res/sub/pixels.bin", 3000, 5, 3)
        else
            assert(contents("/res/sample.ttf") == nil and contents("/res/source.lua") == nil)
            assert(contents("/res/" .. string.rep("x", 31)) == nil)
            assert(contents("/res/sub/pixels.bin") == nil)
            verify("/res/new/image.bin", 3000, 5, 3)
        end
        for _, mode in ipairs({"w", "wb", "a", "r+", "r+b"}) do
            assert(io.open("/res/note.txt", mode) == nil, "write allowed")
        end
        assert(contents("/cli_res_sentinel") == sentinel)
        result = full and "CLI_RES_FULL_PASS" or "CLI_RES_GENERIC_PASS"
        detail = "exact contents, seek, EOF, dir, stats, interleave, readonly, LFS preserved"
    end)
    if not ok then result, detail = "CLI_RES_FAIL", tostring(err) end
    while true do log.info("CLI_RES", result, detail); sys.wait(3000) end
end)
sys.run()
