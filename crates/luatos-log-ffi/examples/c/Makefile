# Makefile for luatos-log-ffi demo (Linux / macOS)
#
# 用法:
#   make          # 编译
#   make test     # 编译并运行
#   make clean

CC ?= cc
CFLAGS ?= -O2 -Wall -Wextra -std=c99
INCLUDE := -I../../include
LIB := -L../../target/release

.PHONY: all test clean

all: demo

demo: demo.c ../../include/luatos_log.h
	$(CC) $(CFLAGS) $(INCLUDE) -o $@ $< $(LIB) -lluatos_log_ffi

test: demo
	@echo "Running demo (需要 LD_LIBRARY_PATH 指向 luatos_log_ffi 所在目录)..."
	@LD_LIBRARY_PATH=../../target/release ./demo

clean:
	rm -f demo
