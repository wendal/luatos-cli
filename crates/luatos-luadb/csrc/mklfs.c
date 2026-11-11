#include "lfs.h"

#include <errno.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>

#ifdef _WIN32
#include <wchar.h>
#include <windows.h>
#else
#include <dirent.h>
#include <unistd.h>
#endif

typedef struct {
    unsigned char *data;
    size_t len;
    size_t cap;
} MklfsBuffer;

typedef struct {
    unsigned char *data;
    size_t size;
} MklfsFlash;

#ifdef _WIN32
static wchar_t *wide_path(const char *path) {
    int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, NULL, 0);
    if (!count) return NULL;
    wchar_t *result = (wchar_t *)malloc(count * sizeof(wchar_t));
    if (result && !MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, result, count)) {
        free(result);
        return NULL;
    }
    return result;
}
#endif

static void buffer_init(MklfsBuffer *buffer) {
    buffer->data = NULL;
    buffer->len = 0;
    buffer->cap = 0;
}

static int buffer_append(MklfsBuffer *buffer, const void *data, size_t size) {
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
        size_t new_cap = buffer->cap == 0 ? 256 : buffer->cap;
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

static void set_error(MklfsBuffer *error, const char *format, ...) {
    char message[1024];
    va_list args;
    int len;
    va_start(args, format);
    len = vsnprintf(message, sizeof(message), format, args);
    va_end(args);
    if (len < 0) {
        buffer_append(error, "unknown mklfs error", strlen("unknown mklfs error"));
        return;
    }
    buffer_append(error, message, (size_t)len < sizeof(message) ? (size_t)len : sizeof(message) - 1);
}

static int flash_range(const MklfsFlash *flash, uint64_t addr, lfs_size_t size) {
    return addr <= flash->size && size <= flash->size - (size_t)addr;
}

static int lfs_read_cb(const struct lfs_config *c, lfs_block_t block,
                       lfs_off_t off, void *buffer, lfs_size_t size) {
    MklfsFlash *flash = (MklfsFlash *)c->context;
    uint64_t addr = (uint64_t)block * c->block_size + off;
    if (!flash_range(flash, addr, size)) return LFS_ERR_IO;
    memcpy(buffer, flash->data + (size_t)addr, size);
    return 0;
}

static int lfs_prog_cb(const struct lfs_config *c, lfs_block_t block,
                       lfs_off_t off, const void *buffer, lfs_size_t size) {
    MklfsFlash *flash = (MklfsFlash *)c->context;
    uint64_t addr = (uint64_t)block * c->block_size + off;
    if (!flash_range(flash, addr, size)) return LFS_ERR_IO;
    memcpy(flash->data + (size_t)addr, buffer, size);
    return 0;
}

static int lfs_erase_cb(const struct lfs_config *c, lfs_block_t block) {
    MklfsFlash *flash = (MklfsFlash *)c->context;
    uint64_t addr = (uint64_t)block * c->block_size;
    if (!flash_range(flash, addr, c->block_size)) return LFS_ERR_IO;
    memset(flash->data + (size_t)addr, 0xFF, c->block_size);
    return 0;
}

static int lfs_sync_cb(const struct lfs_config *c) {
    (void)c;
    return 0;
}

static int create_file(lfs_t *lfs, const char *lfs_path, const char *host_path, MklfsBuffer *error) {
#ifdef _WIN32
    wchar_t *wide = wide_path(host_path);
    FILE *fp = wide ? _wfopen(wide, L"rb") : NULL;
    free(wide);
#else
    FILE *fp = fopen(host_path, "rb");
#endif
    if (!fp) {
        set_error(error, "cannot open %s: %s", host_path, strerror(errno));
        return -1;
    }

    lfs_file_t file;
    int err = lfs_file_open(lfs, &file, lfs_path, LFS_O_WRONLY | LFS_O_CREAT | LFS_O_TRUNC);
    if (err) {
        set_error(error, "lfs_file_open(%s) failed: %d", lfs_path, err);
        fclose(fp);
        return err;
    }

    unsigned char buf[4096];
    size_t n;
    while ((n = fread(buf, 1, sizeof(buf), fp)) > 0) {
        lfs_ssize_t written = lfs_file_write(lfs, &file, buf, (lfs_size_t)n);
        if (written != (lfs_ssize_t)n) {
            set_error(error, "lfs_file_write(%s) failed: %d of %u bytes", lfs_path, (int)written, (unsigned)n);
            lfs_file_close(lfs, &file);
            fclose(fp);
            return written < 0 ? (int)written : -1;
        }
    }
    if (ferror(fp)) {
        set_error(error, "failed to read %s", host_path);
        lfs_file_close(lfs, &file);
        fclose(fp);
        return -1;
    }

    err = lfs_file_close(lfs, &file);
    if (err) {
        set_error(error, "lfs_file_close(%s) failed: %d", lfs_path, err);
        fclose(fp);
        return err;
    }
    if (fseek(fp, 0, SEEK_SET) != 0) {
        set_error(error, "failed to rewind %s", host_path);
        fclose(fp);
        return -1;
    }

    err = lfs_file_open(lfs, &file, lfs_path, LFS_O_RDONLY);
    if (err) {
        set_error(error, "verification open(%s) failed: %d", lfs_path, err);
        fclose(fp);
        return err;
    }
    unsigned char check[4096];
    while ((n = fread(buf, 1, sizeof(buf), fp)) > 0) {
        lfs_ssize_t read = lfs_file_read(lfs, &file, check, (lfs_size_t)n);
        if (read != (lfs_ssize_t)n || memcmp(buf, check, n) != 0) {
            set_error(error, "verification failed for %s", lfs_path);
            lfs_file_close(lfs, &file);
            fclose(fp);
            return -1;
        }
    }
    if (ferror(fp) || lfs_file_read(lfs, &file, check, 1) != 0) {
        set_error(error, "verification length mismatch for %s", lfs_path);
        lfs_file_close(lfs, &file);
        fclose(fp);
        return -1;
    }
    err = lfs_file_close(lfs, &file);
    fclose(fp);
    if (err) {
        set_error(error, "verification close(%s) failed: %d", lfs_path, err);
        return err;
    }
    return 0;
}

static int compact(lfs_t *lfs, const char *lfs_prefix, const char *host_dir, MklfsBuffer *error) {
#ifdef _WIN32
    WIN32_FIND_DATAW fdata;
    char pattern[1024];
    if (strlen(host_dir) + 3 > sizeof(pattern)) {
        set_error(error, "host path too long: %s", host_dir);
        return -1;
    }
    snprintf(pattern, sizeof(pattern), "%s\\*", host_dir);
    wchar_t *wide = wide_path(pattern);
    HANDLE hFind = wide ? FindFirstFileW(wide, &fdata) : INVALID_HANDLE_VALUE;
    free(wide);
    if (hFind == INVALID_HANDLE_VALUE) {
        set_error(error, "cannot open directory %s", host_dir);
        return -1;
    }
    do {
        if (wcscmp(fdata.cFileName, L".") == 0 || wcscmp(fdata.cFileName, L"..") == 0)
            continue;

        char name[1024];
        if (!WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, fdata.cFileName, -1, name, sizeof(name), NULL, NULL)) {
            set_error(error, "cannot convert filename to UTF-8 in %s", host_dir);
            FindClose(hFind);
            return -1;
        }
        if (strlen(host_dir) + strlen(name) + 2 > 1024 || strlen(lfs_prefix) + strlen(name) + 2 > 1024) {
            set_error(error, "path too long under %s", host_dir);
            FindClose(hFind);
            return -1;
        }

        char host_path[1024];
        char lfs_path[1024];
        snprintf(host_path, sizeof(host_path), "%s\\%s", host_dir, name);
        if (strlen(lfs_prefix) == 0)
            snprintf(lfs_path, sizeof(lfs_path), "/%s", name);
        else
            snprintf(lfs_path, sizeof(lfs_path), "%s/%s", lfs_prefix, name);

        if (fdata.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
            int err = lfs_mkdir(lfs, lfs_path);
            if (err && err != LFS_ERR_EXIST) {
                set_error(error, "lfs_mkdir(%s) failed: %d", lfs_path, err);
                FindClose(hFind);
                return err;
            }
            err = compact(lfs, lfs_path, host_path, error);
            if (err) { FindClose(hFind); return err; }
        } else {
            int err = create_file(lfs, lfs_path, host_path, error);
            if (err) { FindClose(hFind); return err; }
        }
    } while (FindNextFileW(hFind, &fdata));
    DWORD last_error = GetLastError();
    FindClose(hFind);
    if (last_error != ERROR_NO_MORE_FILES) {
        set_error(error, "failed to enumerate directory %s: %lu", host_dir, (unsigned long)last_error);
        return -1;
    }
#else
    DIR *dir = opendir(host_dir);
    if (!dir) {
        set_error(error, "cannot open directory %s: %s", host_dir, strerror(errno));
        return -1;
    }
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0)
            continue;

        if (strlen(host_dir) + strlen(entry->d_name) + 2 > 1024 || strlen(lfs_prefix) + strlen(entry->d_name) + 2 > 1024) {
            set_error(error, "path too long under %s", host_dir);
            closedir(dir);
            return -1;
        }
        char host_path[1024];
        char lfs_path[1024];
        snprintf(host_path, sizeof(host_path), "%s/%s", host_dir, entry->d_name);
        if (strlen(lfs_prefix) == 0)
            snprintf(lfs_path, sizeof(lfs_path), "/%s", entry->d_name);
        else
            snprintf(lfs_path, sizeof(lfs_path), "%s/%s", lfs_prefix, entry->d_name);

        struct stat st;
        if (stat(host_path, &st) != 0) continue;

        if (S_ISDIR(st.st_mode)) {
            int err = lfs_mkdir(lfs, lfs_path);
            if (err && err != LFS_ERR_EXIST) {
                set_error(error, "lfs_mkdir(%s) failed: %d", lfs_path, err);
                closedir(dir);
                return err;
            }
            err = compact(lfs, lfs_path, host_path, error);
            if (err) { closedir(dir); return err; }
        } else if (S_ISREG(st.st_mode)) {
            int err = create_file(lfs, lfs_path, host_path, error);
            if (err) { closedir(dir); return err; }
        }
    }
    closedir(dir);
#endif
    return 0;
}

uint32_t mklfs_name_max(void) {
    return LFS_NAME_MAX;
}

int mklfs_build(const char *const *source_dirs, size_t source_dir_count,
                size_t fs_size, size_t block_size,
                uint32_t name_max, uint32_t verify_name_max,
                unsigned char *output, size_t output_len,
                MklfsBuffer *error) {
    size_t i;
    MklfsFlash flash;
    struct lfs_config cfg;
    lfs_t lfs;
    int err;

    if (error == NULL) {
        return 2;
    }
    buffer_init(error);
    if (source_dirs == NULL || source_dir_count == 0 || output == NULL ||
        fs_size == 0 || block_size == 0 || output_len != fs_size ||
        fs_size > UINT32_MAX || block_size > UINT32_MAX ||
        block_size < 256 || block_size % 256 != 0 || fs_size / block_size == 0) {
        set_error(error, "invalid size parameters");
        return 2;
    }
    if (name_max == 0 || name_max > LFS_NAME_MAX || verify_name_max > LFS_NAME_MAX) {
        set_error(error, "invalid name_max (1..%u required)", (unsigned)LFS_NAME_MAX);
        return 2;
    }
    for (i = 0; i < source_dir_count; i++) {
        if (source_dirs[i] == NULL) {
            set_error(error, "invalid source directory");
            return 2;
        }
    }

    flash.data = output;
    flash.size = fs_size;
    memset(flash.data, 0xFF, flash.size);

    memset(&cfg, 0, sizeof(cfg));
    cfg.context = &flash;
    cfg.read = lfs_read_cb;
    cfg.prog = lfs_prog_cb;
    cfg.erase = lfs_erase_cb;
    cfg.sync = lfs_sync_cb;
    cfg.read_size = 256;
    cfg.prog_size = 256;
    cfg.block_size = (lfs_size_t)block_size;
    cfg.block_count = (lfs_size_t)(fs_size / block_size);
    cfg.cache_size = 256;
    cfg.lookahead_size = 16;
    cfg.block_cycles = 500;
    cfg.name_max = name_max;

    err = lfs_format(&lfs, &cfg);
    if (err) {
        set_error(error, "lfs_format failed: %d", err);
        return 1;
    }

    err = lfs_mount(&lfs, &cfg);
    if (err) {
        set_error(error, "lfs_mount failed: %d", err);
        return 1;
    }

    for (i = 0; i < source_dir_count; i++) {
        err = compact(&lfs, "", source_dirs[i], error);
        if (err) {
            lfs_unmount(&lfs);
            return 1;
        }
    }

    lfs_unmount(&lfs);

    if (verify_name_max) {
        lfs_t check_lfs;
        memset(&check_lfs, 0, sizeof(check_lfs));
        cfg.name_max = verify_name_max;
        err = lfs_mount(&check_lfs, &cfg);
        if (err) {
            set_error(error, "firmware-profile mount failed: %d (name_max=%u)", err, (unsigned)verify_name_max);
            return 1;
        }
        lfs_unmount(&check_lfs);
    }
    return 0;
}

void mklfs_free(MklfsBuffer *buffer) {
    if (buffer != NULL) {
        free(buffer->data);
        buffer_init(buffer);
    }
}
