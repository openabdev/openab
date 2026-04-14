@echo off
setlocal
set BASEDIR=C:\Users\Administrator\openab
set LOCKFILE=%BASEDIR%\healthcheck-maintenance.lock

echo [1/4] Setting maintenance lock...
echo %DATE% %TIME% > "%LOCKFILE%"

echo [2/4] Killing all bots...
taskkill /f /im openab.exe >nul 2>&1
taskkill /f /im openab-upstream.exe >nul 2>&1
for /f "tokens=2" %%p in ('wmic process where "Name='cmd.exe' AND CommandLine LIKE '%%run-openab%%'" get ProcessId /value 2^>nul ^| findstr ProcessId') do taskkill /f /pid %%p >nul 2>&1
for /f "tokens=2" %%p in ('wmic process where "Name='node.exe' AND (CommandLine LIKE '%%claude-agent%%' OR CommandLine LIKE '%%copilot-agent%%' OR CommandLine LIKE '%%codex-acp%%' OR CommandLine LIKE '%%gemini%%')" get ProcessId /value 2^>nul ^| findstr ProcessId') do taskkill /f /pid %%p >nul 2>&1
%SYSTEMROOT%\System32\timeout.exe /t 3 /nobreak >nul

echo [3/4] Building...
cd /d %BASEDIR%
cargo build --release
if errorlevel 1 (
    echo BUILD FAILED — removing lock, not restarting.
    del "%LOCKFILE%" >nul 2>&1
    exit /b 1
)

echo [4/4] Restarting all bots...
wscript.exe "%BASEDIR%\run-hidden.vbs" "%BASEDIR%\run-openab-claude.bat"
wscript.exe "%BASEDIR%\run-hidden.vbs" "%BASEDIR%\run-openab-copilot.bat"
wscript.exe "%BASEDIR%\run-hidden.vbs" "%BASEDIR%\run-openab-gemini.bat"
wscript.exe "%BASEDIR%\run-hidden.vbs" "%BASEDIR%\run-openab-codex.bat"
wscript.exe "%BASEDIR%\run-hidden.vbs" "%BASEDIR%\run-openab-copilot-native.bat"

echo Removing maintenance lock...
del "%LOCKFILE%" >nul 2>&1

echo Done! All bots restarted.
