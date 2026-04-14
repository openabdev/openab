# OpenAB Healthcheck Watchdog
# Detects zombie bots via TCP connection check (Discord gateway uses WSS on port 443).
# A healthy bot has 1+ established TCP 443 connections. Zero = zombie.
# Runs every 2 min via Task Scheduler. Silent when healthy.

$ErrorActionPreference = "SilentlyContinue"
$baseDir = "C:\Users\Administrator\openab"
$logFile = "$baseDir\logs\healthcheck.log"
$vbsPath = "$baseDir\run-hidden.vbs"
$cooldownMin = 3
$maintenanceLock = "$baseDir\healthcheck-maintenance.lock"

# ---- Maintenance Lock ----
# If lock file exists, skip all checks (build in progress).
# Auto-expire after 10 minutes to prevent forgotten locks.
if (Test-Path $maintenanceLock) {
    $lockAge = (Get-Date) - (Get-Item $maintenanceLock).LastWriteTime
    if ($lockAge.TotalMinutes -lt 10) {
        exit 0
    }
    # Lock expired — remove and continue
    Remove-Item $maintenanceLock -Force
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    Add-Content -Path $logFile -Value "$ts [MAINTENANCE] expired lock removed (>10min)"
}

# ---- VBS Self-Heal ----
if (-not (Test-Path $vbsPath)) {
    @'
Set WShell = CreateObject("WScript.Shell")
WShell.Run Chr(34) & WScript.Arguments(0) & Chr(34), 0, False
'@ | Set-Content -Path $vbsPath -Encoding ASCII
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    Add-Content -Path $logFile -Value "$ts [HEALED] run-hidden.vbs recreated"
}

# ---- Bridge Self-Heal ----
$bridgePath = "$baseDir\vendor\copilot-agent-acp\copilot-agent-acp.js"
if (-not (Test-Path $bridgePath)) {
    # Restore from git stash commit
    $restoreResult = & git -C $baseDir checkout 6dc84ed -- vendor/copilot-agent-acp/copilot-agent-acp.js 2>&1
    if (Test-Path $bridgePath) {
        $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
        Add-Content -Path $logFile -Value "$ts [HEALED] copilot-agent-acp.js restored from git"
    }
}

# ---- Bot health check ----
$bots = @(
    @{ Name="CICX";    Config="config.toml";         LogDir="logs";         Bat="run-openab-claude.bat";  NodeMatch="claude-agent" },
    @{ Name="GITX";    Config="config-copilot.toml"; LogDir="logs-copilot"; Bat="run-openab-copilot.bat"; NodeMatch="copilot-agent" },
    @{ Name="GIMINIX"; Config="config-gemini.toml";  LogDir="logs-gemini";  Bat="run-openab-gemini.bat";  NodeMatch="gemini" },
    @{ Name="CODEX";   Config="config-codex.toml";   LogDir="logs-codex";   Bat="run-openab-codex.bat";   NodeMatch="codex-acp" },
    @{ Name="COPILX";  Config="config-copilot-native.toml"; LogDir="logs-copilot-native"; Bat="run-openab-copilot-native.bat"; NodeMatch="copilot" }
)

foreach ($bot in $bots) {
    $markerPath = "$baseDir\$($bot.LogDir)\healthcheck-restart.marker"
    $batPath = "$baseDir\$($bot.Bat)"
    $configMatch = [regex]::Escape($bot.Config)

    # Skip if cooldown active
    if (Test-Path $markerPath) {
        $markerAge = (Get-Date) - (Get-Item $markerPath).LastWriteTime
        if ($markerAge.TotalMinutes -lt $cooldownMin) { continue }
        Remove-Item $markerPath -Force
    }

    # Find openab.exe or openab-upstream.exe for this bot
    $proc = Get-CimInstance Win32_Process -Filter "Name='openab.exe' OR Name='openab-upstream.exe'" |
        Where-Object { $_.CommandLine -match $configMatch }

    $needsRestart = $false
    $reason = ""

    if (-not $proc) {
        # Process dead — always restart (kill stale bat loops too)
        $needsRestart = $true
        $reason = "process_dead"
    } else {
        # Process alive — check TCP connections (zombie detection)
        $procId = $proc.ProcessId
        $tcpConns = @(Get-NetTCPConnection -OwningProcess $procId -State Established -EA SilentlyContinue |
            Where-Object { $_.RemotePort -eq 443 })

        if ($tcpConns.Count -eq 0) {
            # Zero TCP 443 = Discord gateway disconnected = zombie
            $needsRestart = $true
            $reason = "zombie(tcp443=0)"
        }
    }

    if (-not $needsRestart) { continue }

    # ---- Kill process tree ----
    $killedPid = 0
    if ($proc) {
        $killedPid = $proc.ProcessId
        Stop-Process -Id $proc.ProcessId -Force -ErrorAction SilentlyContinue
    }

    # Kill ALL bat loops for this bot — match by bat name in CommandLine
    $batMatch = [regex]::Escape($bot.Bat)
    Get-CimInstance Win32_Process -Filter "Name='cmd.exe'" |
        Where-Object { $_.CommandLine -match $batMatch } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

    # Kill node.exe ACP agents
    if ($bot.NodeMatch) {
        Get-CimInstance Win32_Process -Filter "Name='node.exe'" |
            Where-Object { $_.CommandLine -match $bot.NodeMatch } |
            ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    }

    Start-Sleep -Seconds 2

    # Restart via VBS
    if (Test-Path $vbsPath) {
        & wscript.exe $vbsPath $batPath
    }

    # Set cooldown
    Get-Date -Format "o" | Set-Content $markerPath

    # Log
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    Add-Content -Path $logFile -Value "$ts [RESTART] $($bot.Name) $reason pid=$killedPid"
}
