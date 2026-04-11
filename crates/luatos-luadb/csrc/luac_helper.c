#include "lua.h"
#include "lauxlib.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int write_stdout(lua_State *L, const void *p, size_t sz, void *ud) {
    (void)L;
    FILE *out = (FILE *)ud;
    return fwrite(p, 1, sz, out) == sz ? 0 : 1;
}

static unsigned char *read_stdin(size_t *out_len) {
    size_t cap = 8192;
    size_t len = 0;
    unsigned char *buf = (unsigned char *)malloc(cap);
    if (buf == NULL) {
        return NULL;
    }

    for (;;) {
        if (len == cap) {
            size_t new_cap = cap * 2;
            unsigned char *new_buf = (unsigned char *)realloc(buf, new_cap);
            if (new_buf == NULL) {
                free(buf);
                return NULL;
            }
            buf = new_buf;
            cap = new_cap;
        }

        size_t n = fread(buf + len, 1, cap - len, stdin);
        len += n;
        if (n == 0) {
            if (ferror(stdin)) {
                free(buf);
                return NULL;
            }
            break;
        }
    }

    *out_len = len;
    return buf;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s <chunk_name> <strip>\n", argv[0]);
        return 2;
    }

    const char *chunk_name = argv[1];
    int strip = atoi(argv[2]) != 0;

    size_t source_len = 0;
    unsigned char *source = read_stdin(&source_len);
    if (source == NULL) {
        fprintf(stderr, "failed to read source from stdin\n");
        return 1;
    }

    lua_State *L = luaL_newstate();
    if (L == NULL) {
        free(source);
        fprintf(stderr, "failed to create Lua state\n");
        return 1;
    }

    int status = luaL_loadbufferx(L, (const char *)source, source_len, chunk_name, NULL);
    free(source);
    if (status != LUA_OK) {
        size_t len = 0;
        const char *msg = lua_tolstring(L, -1, &len);
        if (msg != NULL) {
            fwrite(msg, 1, len, stderr);
            fputc('\n', stderr);
        } else {
            fprintf(stderr, "unknown compilation error\n");
        }
        lua_close(L);
        return 1;
    }

    if (lua_dump(L, write_stdout, stdout, strip) != 0) {
        fprintf(stderr, "lua_dump failed\n");
        lua_close(L);
        return 1;
    }

    lua_close(L);
    return 0;
}
