/*
 * mklfs_helper — Build a LittleFS image from a directory, output to stdout.
 *
 * Usage: mklfs_helper <source_dir> <fs_size_bytes> [block_size] [read_size] [prog_size]
 *
 * Defaults:
 *   block_size = 4096
 *   read_size  = 256
 *   prog_size  = 256
 *
 * Based on mklfs from luatos-soc-air101.
 */

#include "lfs.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <sys/types.h>
#include <sys/stat.h>

#ifdef _WIN32
#include <io.h>
#include <fcntl.h>
#include <windows.h>
#include <wchar.h>
#else
#include <dirent.h>
#include <unistd.h>
#endif

/* In-memory flash buffer */
static unsigned char *flash_buf = NULL;
static lfs_size_t flash_size = 0;

#ifdef _WIN32
/* Windows file APIs use UTF-16; filesystem entry names stay UTF-8 in LittleFS.
 * POSIX builds use their existing byte-path fopen/opendir implementation. */
static wchar_t *wide_path(const char *path)
{
    int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, NULL, 0);
    if (!count) return NULL;
    wchar_t *result = (wchar_t *)malloc(count * sizeof(wchar_t));
    if (result && !MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, result, count)) { free(result); return NULL; }
    return result;
}
#endif

static int lfs_read_cb(const struct lfs_config *c, lfs_block_t block,
                       lfs_off_t off, void *buffer, lfs_size_t size)
{
    (void)c;
    lfs_size_t addr = block * c->block_size + off;
    if (addr + size > flash_size) return LFS_ERR_IO;
    memcpy(buffer, flash_buf + addr, size);
    return 0;
}

static int lfs_prog_cb(const struct lfs_config *c, lfs_block_t block,
                       lfs_off_t off, const void *buffer, lfs_size_t size)
{
    (void)c;
    lfs_size_t addr = block * c->block_size + off;
    if (addr + size > flash_size) return LFS_ERR_IO;
    memcpy(flash_buf + addr, buffer, size);
    return 0;
}

static int lfs_erase_cb(const struct lfs_config *c, lfs_block_t block)
{
    (void)c;
    lfs_size_t addr = block * c->block_size;
    if (addr + c->block_size > flash_size) return LFS_ERR_IO;
    memset(flash_buf + addr, 0xFF, c->block_size);
    return 0;
}

static int lfs_sync_cb(const struct lfs_config *c)
{
    (void)c;
    return 0;
}

/* Create a file in the LFS from a host file */
static int create_file(lfs_t *lfs, const char *lfs_path, const char *host_path)
{
#ifdef _WIN32
    wchar_t *wide = wide_path(host_path);
    FILE *fp = wide ? _wfopen(wide, L"rb") : NULL;
    free(wide);
#else
    FILE *fp = fopen(host_path, "rb");
#endif
    if (!fp) {
        fprintf(stderr, "cannot open %s: %s\n", host_path, strerror(errno));
        return -1;
    }

    lfs_file_t file;
    int err = lfs_file_open(lfs, &file, lfs_path, LFS_O_WRONLY | LFS_O_CREAT | LFS_O_TRUNC);
    if (err) {
        fprintf(stderr, "lfs_file_open(%s) failed: %d\n", lfs_path, err);
        fclose(fp);
        return err;
    }

    unsigned char buf[4096];
    size_t n;
    while ((n = fread(buf, 1, sizeof(buf), fp)) > 0) {
        lfs_ssize_t written = lfs_file_write(lfs, &file, buf, (lfs_size_t)n);
        if (written != (lfs_ssize_t)n) {
            fprintf(stderr, "lfs_file_write(%s) failed: %d\n", lfs_path, (int)written);
            lfs_file_close(lfs, &file);
            fclose(fp);
            return (int)written;
        }
    }

    int failed = ferror(fp);
    err = lfs_file_close(lfs, &file);
    if (failed || err) { fclose(fp); return -1; }
    rewind(fp);
    err = lfs_file_open(lfs, &file, lfs_path, LFS_O_RDONLY);
    if (err) { fclose(fp); return err; }
    unsigned char check[4096];
    while ((n = fread(buf, 1, sizeof(buf), fp)) > 0) {
        if (lfs_file_read(lfs, &file, check, (lfs_size_t)n) != (lfs_ssize_t)n || memcmp(buf, check, n)) {
            lfs_file_close(lfs, &file); fclose(fp); return -1;
        }
    }
    failed = ferror(fp) || lfs_file_read(lfs, &file, check, 1) != 0;
    err = lfs_file_close(lfs, &file);
    fclose(fp);
    return failed || err ? -1 : 0;
}

/* Recursively pack a directory into LFS */
static int compact(lfs_t *lfs, const char *lfs_prefix, const char *host_dir)
{
#ifdef _WIN32
    WIN32_FIND_DATAW fdata;
    char pattern[1024];
    snprintf(pattern, sizeof(pattern), "%s\\*", host_dir);
    wchar_t *wide = wide_path(pattern);
    HANDLE hFind = wide ? FindFirstFileW(wide, &fdata) : INVALID_HANDLE_VALUE;
    free(wide);
    if (hFind == INVALID_HANDLE_VALUE) {
        fprintf(stderr, "cannot open directory %s\n", host_dir);
        return -1;
    }
    do {
        if (wcscmp(fdata.cFileName, L".") == 0 || wcscmp(fdata.cFileName, L"..") == 0)
            continue;

        char name[1024];
        if (!WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, fdata.cFileName, -1, name, sizeof(name), NULL, NULL)) { FindClose(hFind); return -1; }
        if (strlen(host_dir) + strlen(name) + 2 > 1024 || strlen(lfs_prefix) + strlen(name) + 2 > 1024) { FindClose(hFind); return -1; }

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
                fprintf(stderr, "lfs_mkdir(%s) failed: %d\n", lfs_path, err);
                FindClose(hFind);
                return err;
            }
            err = compact(lfs, lfs_path, host_path);
            if (err) { FindClose(hFind); return err; }
        } else {
            int err = create_file(lfs, lfs_path, host_path);
            if (err) { FindClose(hFind); return err; }
        }
    } while (FindNextFileW(hFind, &fdata));
    DWORD last_error = GetLastError();
    FindClose(hFind);
    if (last_error != ERROR_NO_MORE_FILES) return -1;
#else
    DIR *dir = opendir(host_dir);
    if (!dir) {
        fprintf(stderr, "cannot open directory %s: %s\n", host_dir, strerror(errno));
        return -1;
    }
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0)
            continue;

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
                fprintf(stderr, "lfs_mkdir(%s) failed: %d\n", lfs_path, err);
                closedir(dir);
                return err;
            }
            err = compact(lfs, lfs_path, host_path);
            if (err) { closedir(dir); return err; }
        } else if (S_ISREG(st.st_mode)) {
            int err = create_file(lfs, lfs_path, host_path);
            if (err) { closedir(dir); return err; }
        }
    }
    closedir(dir);
#endif
    return 0;
}

static void print_usage(const char *prog)
{
    fprintf(stderr, "usage: %s <source_dir> <fs_size_bytes> [block_size] [read_size] [prog_size]\n", prog);
    fprintf(stderr, "       %s --merge <fs_size_bytes> <block_size> <read_size> <prog_size> <dir> [dir...]\n", prog);
    fprintf(stderr, "  --merge packs directories in order; later directories override same-path files.\n");
    fprintf(stderr, "  Optional prefix: --name-max <N> --verify-name-max <N>\n");
}

static int mklfs_main(int argc, char **argv)
{
#ifdef _WIN32
    _setmode(_fileno(stdout), _O_BINARY);
#endif

    lfs_size_t name_max = 0;
    lfs_size_t verify_name_max = 0;
    while (argc >= 2 && (strcmp(argv[1], "--name-max") == 0 || strcmp(argv[1], "--verify-name-max") == 0)) {
        if (argc < 3) return 2;
        char *end = NULL;
        unsigned long value = strtoul(argv[2], &end, 10);
        if (!end || *end || value == 0 || value > LFS_NAME_MAX) {
            fprintf(stderr, "invalid name_max (1..%u required)\n", (unsigned)LFS_NAME_MAX);
            return 2;
        }
        if (strcmp(argv[1], "--name-max") == 0) name_max = (lfs_size_t)value;
        else verify_name_max = (lfs_size_t)value;
        argc -= 2;
        argv += 2;
    }

    const char *single_dir = NULL;
    const char **source_dirs = NULL;
    int source_dir_count = 0;
    lfs_size_t block_size = 4096;
    lfs_size_t read_size  = 256;
    lfs_size_t prog_size  = 256;

    if (argc >= 2 && strcmp(argv[1], "--merge") == 0) {
        if (argc < 7) {
            print_usage(argv[0]);
            return 2;
        }
        flash_size = (lfs_size_t)strtoul(argv[2], NULL, 10);
        block_size = (lfs_size_t)strtoul(argv[3], NULL, 10);
        read_size  = (lfs_size_t)strtoul(argv[4], NULL, 10);
        prog_size  = (lfs_size_t)strtoul(argv[5], NULL, 10);
        source_dirs = (const char **)&argv[6];
        source_dir_count = argc - 6;
    } else {
        if (argc < 3) {
            print_usage(argv[0]);
            return 2;
        }
        single_dir = argv[1];
        flash_size = (lfs_size_t)strtoul(argv[2], NULL, 10);
        if (argc > 3) block_size = (lfs_size_t)strtoul(argv[3], NULL, 10);
        if (argc > 4) read_size  = (lfs_size_t)strtoul(argv[4], NULL, 10);
        if (argc > 5) prog_size  = (lfs_size_t)strtoul(argv[5], NULL, 10);
        source_dirs = &single_dir;
        source_dir_count = 1;
    }

    if (flash_size == 0 || block_size == 0 || read_size == 0 || prog_size == 0) {
        fprintf(stderr, "invalid size parameters\n");
        return 1;
    }

    flash_buf = (unsigned char *)calloc(1, flash_size);
    if (!flash_buf) {
        fprintf(stderr, "cannot allocate %u bytes for flash buffer\n", flash_size);
        return 1;
    }
    memset(flash_buf, 0xFF, flash_size);

    struct lfs_config cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.read  = lfs_read_cb;
    cfg.prog  = lfs_prog_cb;
    cfg.erase = lfs_erase_cb;
    cfg.sync  = lfs_sync_cb;
    cfg.read_size   = read_size;
    cfg.prog_size   = prog_size;
    cfg.block_size  = block_size;
    cfg.block_count = flash_size / block_size;
    cfg.cache_size  = read_size;  /* use read_size for cache */
    cfg.lookahead_size = 16;
    cfg.block_cycles   = 500;
    cfg.name_max       = name_max;

    lfs_t lfs;
    int err = lfs_format(&lfs, &cfg);
    if (err) {
        fprintf(stderr, "lfs_format failed: %d\n", err);
        free(flash_buf);
        return 1;
    }

    err = lfs_mount(&lfs, &cfg);
    if (err) {
        fprintf(stderr, "lfs_mount failed: %d\n", err);
        free(flash_buf);
        return 1;
    }

    for (int i = 0; i < source_dir_count; i++) {
        err = compact(&lfs, "", source_dirs[i]);
        if (err) {
            lfs_unmount(&lfs);
            free(flash_buf);
            return 1;
        }
    }

    lfs_unmount(&lfs);

    /* Firmware may reject a valid image whose superblock advertises a larger
     * name_max than its mount configuration. Never format on validation failure. */
    if (verify_name_max) {
        lfs_t check_lfs;
        memset(&check_lfs, 0, sizeof(check_lfs));
        cfg.name_max = verify_name_max;
        err = lfs_mount(&check_lfs, &cfg);
        if (err) {
            fprintf(stderr, "firmware-profile mount failed: %d (name_max=%u)\n", err, (unsigned)verify_name_max);
            free(flash_buf);
            return 1;
        }
        lfs_unmount(&check_lfs);
    }

    /* Write the flash image to stdout */
    size_t written = fwrite(flash_buf, 1, flash_size, stdout);
    if (written != flash_size) {
        fprintf(stderr, "failed to write output: wrote %zu of %u bytes\n",
                written, flash_size);
        free(flash_buf);
        return 1;
    }
    fflush(stdout);

    free(flash_buf);
    return 0;
}

#ifdef _WIN32
/* Read Unicode command-line paths without conversion through the Windows
 * legacy code page, then share the UTF-8 packing logic with other platforms. */
int wmain(int argc, wchar_t **wide_argv)
{
    char **argv = (char **)calloc(argc + 1, sizeof(char *));
    if (!argv) return 1;
    for (int i = 0; i < argc; i++) {
        int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide_argv[i], -1, NULL, 0, NULL, NULL);
        if (!size || !(argv[i] = (char *)malloc(size)) || !WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, wide_argv[i], -1, argv[i], size, NULL, NULL)) {
            for (int j = 0; j <= i; j++) free(argv[j]);
            free(argv); return 1;
        }
    }
    int status = mklfs_main(argc, argv);
    for (int i = 0; i < argc; i++) free(argv[i]);
    free(argv); return status;
}
#else
int main(int argc, char **argv) { return mklfs_main(argc, argv); }
#endif
