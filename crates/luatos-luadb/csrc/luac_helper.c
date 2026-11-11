#include "lprefix.h"

#include "lua.h"
#include "lauxlib.h"
#include "lobject.h"
#include "lundump.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    unsigned char *data;
    size_t len;
    size_t cap;
} LuacBuffer;

typedef struct {
    const unsigned char *source;
    size_t source_len;
    const char *chunk_name;
    int debug_mode;
    LuacBuffer *output;
} LuacRequest;

static void buffer_init(LuacBuffer *buffer) {
    buffer->data = NULL;
    buffer->len = 0;
    buffer->cap = 0;
}

static void buffer_release(LuacBuffer *buffer) {
    free(buffer->data);
    buffer_init(buffer);
}

static int buffer_append(LuacBuffer *buffer, const void *data, size_t size) {
    size_t required;
    unsigned char *new_data;
    if (size == 0) {
        return 0;
    }
    if (size > SIZE_MAX - buffer->len) {
        return 1;
    }
    required = buffer->len + size;
    if (required > buffer->cap) {
        size_t new_cap = buffer->cap == 0 ? 8192 : buffer->cap;
        while (new_cap < required) {
            if (new_cap > SIZE_MAX / 2) {
                new_cap = required;
                break;
            }
            new_cap *= 2;
        }
        new_data = (unsigned char *)realloc(buffer->data, new_cap);
        if (new_data == NULL) {
            return 1;
        }
        buffer->data = new_data;
        buffer->cap = new_cap;
    }
    memcpy(buffer->data + buffer->len, data, size);
    buffer->len += size;
    return 0;
}

static int write_bytecode(lua_State *L, const void *data, size_t size, void *ud) {
    (void)L;
    return buffer_append((LuacBuffer *)ud, data, size);
}

static void set_error(LuacBuffer *error, const char *message) {
    buffer_append(error, message, strlen(message));
}

static int compile_protected(lua_State *L) {
    LuacRequest *request = (LuacRequest *)lua_touserdata(L, 1);
    int status = luaL_loadbufferx(L, (const char *)request->source, request->source_len, request->chunk_name, NULL);
    const Proto *f;
    if (status != LUA_OK) {
        return lua_error(L);
    }
    f = ((const LClosure *)lua_topointer(L, -1))->p;
    if (luaU_dump_ex(L, f, write_bytecode, request->output, request->debug_mode) != 0) {
        return luaL_error(L, "luaU_dump_ex failed");
    }
    return 0;
}

/* 返回值：0=成功，1=编译失败，2=参数错误。非空输出缓冲区在任何返回后都可安全释放。 */
static int compile_impl(const unsigned char *source, size_t source_len, const char *chunk_name, int debug_mode, LuacBuffer *output, LuacBuffer *error) {
    lua_State *L;
    LuacRequest request;
    int status;
    if (output != NULL) {
        buffer_init(output);
    }
    if (error != NULL) {
        buffer_init(error);
    }
    if (output == NULL || error == NULL) {
        if (error != NULL) {
            set_error(error, "invalid compiler argument");
        }
        return 2;
    }
    if ((source == NULL && source_len != 0) || chunk_name == NULL) {
        set_error(error, "invalid compiler argument");
        return 2;
    }

    L = luaL_newstate();
    if (L == NULL) {
        set_error(error, "failed to create Lua state");
        return 1;
    }
    request.source = source;
    request.source_len = source_len;
    request.chunk_name = chunk_name;
    request.debug_mode = debug_mode;
    request.output = output;

    lua_pushcfunction(L, compile_protected);
    lua_pushlightuserdata(L, &request);
    status = lua_pcall(L, 1, 0, 0);
    if (status != LUA_OK) {
        size_t len = 0;
        const char *message = lua_tolstring(L, -1, &len);
        if (message == NULL) {
            set_error(error, "unknown compilation error");
        } else {
            buffer_append(error, message, len);
        }
        buffer_release(output);
    }
    lua_close(L);
    return status == LUA_OK ? 0 : 1;
}

#ifdef LUA_32BITS
#define LUAC_COMPILE luac32_compile
#define LUAC_FREE luac32_free
#else
#define LUAC_COMPILE luac64_compile
#define LUAC_FREE luac64_free
#endif

int LUAC_COMPILE(const unsigned char *source, size_t source_len, const char *chunk_name, int debug_mode, LuacBuffer *output, LuacBuffer *error) {
    return compile_impl(source, source_len, chunk_name, debug_mode, output, error);
}

void LUAC_FREE(LuacBuffer *buffer) {
    if (buffer != NULL) {
        buffer_release(buffer);
    }
}
