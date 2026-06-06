@echo off
REM build.bat — Windows MSVC build for luatos-log-ffi demo
REM
REM 用法 (在 "x64 Native Tools Command Prompt for VS" 中):
REM   build.bat

if not exist ..\..\target\release\luatos_log_ffi.dll (
    echo Error: ../../target/release/luatos_log_ffi.dll not found.
    echo Run: cargo build --release -p luatos-log-ffi
    exit /b 1
)

cl /nologo /W4 /std:c11 /I..\..\include /Fe:demo.exe demo.c /link ..\..\target\release\luatos_log_ffi.lib
if errorlevel 1 exit /b 1

echo Build OK. Run: demo.exe
