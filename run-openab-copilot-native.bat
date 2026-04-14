@echo off
setlocal
set DISCORD_BOT_TOKEN=REDACTED_COPILX_TOKEN
set PATH=%APPDATA%\npm;%LOCALAPPDATA%\Microsoft\WinGet\Links;%PATH%
cd /d C:\Users\Administrator\openab

rem ---- log rotate (keep last 5) ----
set LOGDIR=C:\Users\Administrator\openab\logs-copilot-native
if not exist "%LOGDIR%" mkdir "%LOGDIR%"
if exist "%LOGDIR%\openab.log.5" del "%LOGDIR%\openab.log.5" 2>nul
if exist "%LOGDIR%\openab.log.4" ren "%LOGDIR%\openab.log.4" "openab.log.5" 2>nul
if exist "%LOGDIR%\openab.log.3" ren "%LOGDIR%\openab.log.3" "openab.log.4" 2>nul
if exist "%LOGDIR%\openab.log.2" ren "%LOGDIR%\openab.log.2" "openab.log.3" 2>nul
if exist "%LOGDIR%\openab.log.1" ren "%LOGDIR%\openab.log.1" "openab.log.2" 2>nul
if exist "%LOGDIR%\openab.log"   ren "%LOGDIR%\openab.log"   "openab.log.1" 2>nul

rem ---- mutex: only one bat loop per bot ----
set LOCKFILE=%LOGDIR%\loop.lock
2>nul (
  9>"%LOCKFILE%" (
    :loop
    "C:\Users\Administrator\openab\target\release\openab.exe" config-copilot-native.toml >> "%LOGDIR%\openab.log" 2>&1
    %SYSTEMROOT%\System32\timeout.exe /t 5 /nobreak > nul
    goto loop
  )
) || (
  exit /b 0
)
