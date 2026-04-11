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
#else
#include <dirent.h>
#include <unistd.h>
#endif

/* In-memory flash buffer */
static unsigned char *flash_buf = NULL;
static lfs_size_t flash_size = 0;

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
    FILE *fp = fopen(host_path, "rb");
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
        if (written < 0) {
            fprintf(stderr, "lfs_file_write(%s) failed: %d\n", lfs_path, (int)written);
            lfs_file_close(lfs, &file);
            fclose(fp);
            return (int)written;
        }
    }

    lfs_file_close(lfs, &file);
    fclose(fp);
    return 0;
}

/* Recursively pack a directory into LFS */
static int compact(lfs_t *lfs, const char *lfs_prefix, const char *host_dir)
{
#ifdef _WIN32
    WIN32_FIND_DATAA fdata;
    char pattern[1024];
    snprintf(pattern, sizeof(pattern), "%s\\*", host_dir);
    HANDLE hFind = FindFirstFileA(pattern, &fdata);
    if (hFind == INVALID_HANDLE_VALUE) {
        fprintf(stderr, "cannot open directory %s\n", host_dir);
        return -1;
    }
    do {
        if (strcmp(fdata.cFileName, ".") == 0 || strcmp(fdata.cFileName, "..") == 0)
            continue;

        char host_path[1024];
        char lfs_path[1024];
        snprintf(host_path, sizeof(host_path), "%s\\%s", host_dir, fdata.cFileName);
        if (strlen(lfs_prefix) == 0)
            snprintf(lfs_path, sizeof(lfs_path), "/%s", fdata.cFileName);
        else
            snprintf(lfs_path, sizeof(lfs_path), "%s/%s", lfs_prefix, fdata.cFileName);

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
    } while (FindNextFileA(hFind, &fdata));
    FindClose(hFind);
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

int main(int argc, char **argv)
{
#ifdef _WIN32
    _setmode(_fileno(stdout), _O_BINARY);
#endif

    if (argc < 3) {
        fprintf(stderr, "usage: %s <source_dir> <fs_size_bytes> [block_size] [read_size] [prog_size]\n", argv[0]);
        return 2;
    }

    const char *source_dir = argv[1];
    flash_size = (lfs_size_t)strtoul(argv[2], NULL, 10);
    lfs_size_t block_size = 4096;
    lfs_size_t read_size  = 256;
    lfs_size_t prog_size  = 256;

    if (argc > 3) block_size = (lfs_size_t)strtoul(argv[3], NULL, 10);
    if (argc > 4) read_size  = (lfs_size_t)strtoul(argv[4], NULL, 10);
    if (argc > 5) prog_size  = (lfs_size_t)strtoul(argv[5], NULL, 10);

    if (flash_size == 0 || block_size == 0) {
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

    err = compact(&lfs, "", source_dir);
    if (err) {
        lfs_unmount(&lfs);
        free(flash_buf);
        return 1;
    }

    lfs_unmount(&lfs);

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
