@echo off
setlocal enabledelayedexpansion

:: Find herdr.exe
set "HERDR="
for %%d in (
    "%USERPROFILE%\.herdr\bin\herdr.exe"
    "%USERPROFILE%\herdr.exe"
    "%LOCALAPPDATA%\herdr\herdr.exe"
    "%ProgramFiles%\herdr\herdr.exe"
) do (
    if exist %%d set "HERDR=%%d"
)

:: Fallback: check PATH
if not defined HERDR (
    for /f "delims=" %%a in ('where herdr.exe 2^>nul') do set "HERDR=%%a"
)

if not defined HERDR (
    echo [Herdr] Binary not found. Install: irm https://herdr.dev/install.ps1 ^| iex
    exit /b 1
)

if "%1"=="--version" (
    "!HERDR!" --version
    exit /b 0
)
if "%1"=="version" (
    "!HERDR!" --version
    exit /b 0
)
if "%1"=="update" (
    "!HERDR!" update
    exit /b 0
)

"!HERDR!" %*
