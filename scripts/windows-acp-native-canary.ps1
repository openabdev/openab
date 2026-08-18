[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OpenAbExe,

    [Parameter(Mandatory = $true)]
    [string]$AgentExe,

    [string]$WorkDir = (Get-Location).Path
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-Canary {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw "CANARY FAIL: $Message"
    }
}

function Add-BaselineEnvironment {
    param([System.Diagnostics.ProcessStartInfo]$StartInfo)
    $StartInfo.EnvironmentVariables.Clear()
    foreach ($name in @(
        "SystemRoot", "SystemDrive", "PATH", "PATHEXT", "TEMP", "TMP",
        "USERPROFILE", "USERNAME", "HOME", "APPDATA", "LOCALAPPDATA"
    )) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            $StartInfo.EnvironmentVariables[$name] = $value
        }
    }
}

function New-RedirectedProcess {
    param(
        [string]$FilePath,
        [string[]]$ArgumentList,
        [hashtable]$Environment,
        [string]$WorkingDirectory
    )
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FilePath
    $start.WorkingDirectory = $WorkingDirectory
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    Add-BaselineEnvironment $start
    # ProcessStartInfo.ArgumentList is unavailable in Windows PowerShell 5.1.
    # Windows paths cannot contain a literal double quote, so quoting every
    # argument gives one implementation that works on both 5.1 and PowerShell 7.
    Assert-Canary -Condition (-not ($ArgumentList | Where-Object { $_.Contains('"') })) -Message "argument contains a double quote"
    $start.Arguments = (($ArgumentList | ForEach-Object { '"' + $_ + '"' }) -join " ")
    foreach ($entry in $Environment.GetEnumerator()) {
        $start.EnvironmentVariables[$entry.Key] = [string]$entry.Value
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $start
    Assert-Canary -Condition ($process.Start()) -Message "could not start $FilePath"
    return $process
}

function Send-StdioJson {
    param([System.Diagnostics.Process]$Process, [hashtable]$Frame)
    $line = $Frame | ConvertTo-Json -Compress -Depth 20
    $Process.StandardInput.WriteLine($line)
    $Process.StandardInput.Flush()
}

function Receive-StdioJson {
    param([System.Diagnostics.Process]$Process, [int]$TimeoutSeconds = 15)
    $read = $Process.StandardOutput.ReadLineAsync()
    Assert-Canary -Condition ($read.Wait([TimeSpan]::FromSeconds($TimeoutSeconds))) -Message "stdio response timed out"
    Assert-Canary -Condition ($null -ne $read.Result) -Message "stdio closed before response"
    return $read.Result | ConvertFrom-Json
}

function Send-WebSocketJson {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, [hashtable]$Frame)
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Frame | ConvertTo-Json -Compress -Depth 20))
    $segment = [ArraySegment[byte]]::new($bytes)
    $Socket.SendAsync(
        $segment,
        [System.Net.WebSockets.WebSocketMessageType]::Text,
        $true,
        [Threading.CancellationToken]::None
    ).GetAwaiter().GetResult()
}

function Receive-WebSocketJson {
    param([System.Net.WebSockets.ClientWebSocket]$Socket, [int]$TimeoutSeconds = 15)
    $buffer = [byte[]]::new(65536)
    $stream = [System.IO.MemoryStream]::new()
    $cancel = [Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds($TimeoutSeconds))
    try {
        do {
            $segment = [ArraySegment[byte]]::new($buffer)
            $result = $Socket.ReceiveAsync($segment, $cancel.Token).GetAwaiter().GetResult()
            Assert-Canary -Condition (
                $result.MessageType -ne [System.Net.WebSockets.WebSocketMessageType]::Close
            ) -Message "WebSocket closed before response"
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        return [Text.Encoding]::UTF8.GetString($stream.ToArray()) | ConvertFrom-Json
    }
    finally {
        $cancel.Dispose()
        $stream.Dispose()
    }
}

function Receive-WebSocketResponse {
    param(
        [System.Net.WebSockets.ClientWebSocket]$Socket,
        [int]$Id,
        [int]$TimeoutSeconds = 30
    )
    $watch = [Diagnostics.Stopwatch]::StartNew()
    while ($watch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        $remaining = [Math]::Max(1, $TimeoutSeconds - [int]$watch.Elapsed.TotalSeconds)
        $frame = Receive-WebSocketJson $Socket $remaining
        if (($frame.PSObject.Properties.Name -contains "id") -and [int]$frame.id -eq $Id) {
            return $frame
        }
    }
    throw "CANARY FAIL: no WebSocket response for id $Id"
}

function Wait-TcpPort {
    param([int]$Port, [int]$TimeoutSeconds = 20)
    $watch = [Diagnostics.Stopwatch]::StartNew()
    while ($watch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        $client = [Net.Sockets.TcpClient]::new()
        try {
            $connect = $client.ConnectAsync("127.0.0.1", $Port)
            if ($connect.Wait(250) -and $client.Connected) {
                return
            }
        }
        catch {
        }
        finally {
            $client.Dispose()
        }
        Start-Sleep -Milliseconds 100
    }
    throw "CANARY FAIL: OpenAB did not listen on port $Port"
}

function Test-ProcessGone {
    param([int]$Id)
    return $null -eq (Get-Process -Id $Id -ErrorAction SilentlyContinue)
}

$OpenAbExe = (Resolve-Path -LiteralPath $OpenAbExe).Path
$AgentExe = (Resolve-Path -LiteralPath $AgentExe).Path
$WorkDir = (Resolve-Path -LiteralPath $WorkDir).Path

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("openab-windows-canary-" + [Guid]::NewGuid())
$homeDir = Join-Path $tempRoot "home"
$configPath = Join-Path $tempRoot "config.toml"
New-Item -ItemType Directory -Path $homeDir -Force | Out-Null

$agent = $null
$root = $null
$socket = $null
$rootStdout = $null
$rootStderr = $null
$agentPid = $null

try {
    & $AgentExe --help | Out-Null
    Assert-Canary -Condition ($LASTEXITCODE -eq 0) -Message "openab-agent.exe --help"
    & $OpenAbExe --help | Out-Null
    Assert-Canary -Condition ($LASTEXITCODE -eq 0) -Message "openab.exe --help"
    Write-Host "[PASS] both binaries answer --help"

    $isolatedAgentEnv = @{
        HOME = $homeDir
        USERPROFILE = $homeDir
        ANTHROPIC_API_KEY = "ci-non-secret-no-provider-call"
        HTTPS_PROXY = "http://127.0.0.1:9"
        HTTP_PROXY = "http://127.0.0.1:9"
        ALL_PROXY = "http://127.0.0.1:9"
        NO_PROXY = ""
    }
    $agent = New-RedirectedProcess -FilePath $AgentExe -ArgumentList ([string[]]@()) -Environment $isolatedAgentEnv -WorkingDirectory $WorkDir
    Send-StdioJson $agent @{
        jsonrpc = "2.0"
        id = 1
        method = "initialize"
        params = @{ protocolVersion = 1; clientCapabilities = @{}; clientInfo = @{ name = "windows-canary"; version = "1" } }
    }
    $initialize = Receive-StdioJson $agent
    Assert-Canary -Condition ($initialize.id -eq 1 -and $initialize.result.agentInfo.name -eq "openab-agent") -Message "agent initialize"

    Send-StdioJson $agent @{
        jsonrpc = "2.0"
        id = 2
        method = "session/new"
        params = @{ cwd = $WorkDir; mcpServers = @() }
    }
    $newSession = Receive-StdioJson $agent
    Assert-Canary -Condition (
        $newSession.id -eq 2 -and -not [string]::IsNullOrWhiteSpace($newSession.result.sessionId)
    ) -Message "agent session/new"
    $agent.StandardInput.Close()
    Assert-Canary -Condition ($agent.WaitForExit(10000)) -Message "agent did not stop after stdin EOF"
    Assert-Canary -Condition ($agent.ExitCode -eq 0) -Message "agent returned non-zero after stdio smoke"
    Write-Host "[PASS] openab-agent ACP stdio initialize + session/new + EOF shutdown"

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $listener.Stop()

    $agentToml = $AgentExe.Replace("'", "''")
    $workToml = $WorkDir.Replace("'", "''")
    $config = @"
[agent]
command = '$agentToml'
working_dir = '$workToml'
env = { ANTHROPIC_API_KEY = "ci-non-secret-no-provider-call", HTTPS_PROXY = "http://127.0.0.1:9", HTTP_PROXY = "http://127.0.0.1:9", ALL_PROXY = "http://127.0.0.1:9", NO_PROXY = "" }

[pool]
max_sessions = 2
"@
    [IO.File]::WriteAllText($configPath, $config, [Text.UTF8Encoding]::new($false))

    $rootEnv = @{
        HOME = $homeDir
        USERPROFILE = $homeDir
        OPENAB_ACP_ENABLED = "1"
        GATEWAY_LISTEN = "127.0.0.1:$port"
        GATEWAY_ALLOW_ALL_USERS = "true"
        RUST_LOG = "info"
    }
    $root = New-RedirectedProcess -FilePath $OpenAbExe -ArgumentList @("run", "-c", $configPath) -Environment $rootEnv -WorkingDirectory $WorkDir
    $rootStdout = $root.StandardOutput.ReadToEndAsync()
    $rootStderr = $root.StandardError.ReadToEndAsync()
    Wait-TcpPort $port

    $socket = [System.Net.WebSockets.ClientWebSocket]::new()
    $socket.ConnectAsync(
        [Uri]"ws://127.0.0.1:$port/acp",
        [Threading.CancellationToken]::None
    ).GetAwaiter().GetResult()

    Send-WebSocketJson $socket @{
        jsonrpc = "2.0"
        id = 1
        method = "initialize"
        params = @{ protocolVersion = 1; clientCapabilities = @{}; clientInfo = @{ name = "windows-canary"; version = "1" } }
    }
    $rootInitialize = Receive-WebSocketResponse $socket 1
    Assert-Canary -Condition ($rootInitialize.result.protocolVersion -eq 1) -Message "root /acp initialize"

    Send-WebSocketJson $socket @{
        jsonrpc = "2.0"
        id = 2
        method = "session/new"
        params = @{ cwd = $WorkDir; mcpServers = @() }
    }
    $rootNew = Receive-WebSocketResponse $socket 2
    $sessionId = [string]$rootNew.result.sessionId
    Assert-Canary -Condition ($sessionId.StartsWith("sess_")) -Message "root /acp session/new"

    Send-WebSocketJson $socket @{
        jsonrpc = "2.0"
        id = 3
        method = "session/prompt"
        params = @{ sessionId = $sessionId; prompt = @(@{ type = "text"; text = "NO_NETWORK_CANARY" }) }
    }
    $promptResult = Receive-WebSocketResponse $socket 3 45
    $promptSettled = ($promptResult.PSObject.Properties.Name -contains "result") -or
        ($promptResult.PSObject.Properties.Name -contains "error")
    Assert-Canary -Condition $promptSettled -Message "root prompt did not settle"

    $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($root.Id)" |
        Where-Object { $_.Name -ieq "openab-agent.exe" })
    Assert-Canary -Condition ($children.Count -eq 1) -Message "root did not own exactly one openab-agent.exe"
    $agentPid = [int]$children[0].ProcessId

    Send-WebSocketJson $socket @{
        jsonrpc = "2.0"
        id = 4
        method = "session/prompt"
        params = @{ sessionId = $sessionId; prompt = @(@{ type = "text"; text = "CANCEL_CANARY" }) }
    }
    Send-WebSocketJson $socket @{
        jsonrpc = "2.0"
        method = "session/cancel"
        params = @{ sessionId = $sessionId }
    }
    $cancelResult = Receive-WebSocketResponse $socket 4 15
    Assert-Canary -Condition ($cancelResult.result.stopReason -eq "cancelled") -Message "gateway cancellation did not settle as cancelled"
    Write-Host "[PASS] openab.exe /acp -> openab-agent.exe full-chain spawn and gateway cancel"

    $socket.Dispose()
    $socket = $null
    Stop-Process -Id $root.Id -Force
    Assert-Canary -Condition ($root.WaitForExit(10000)) -Message "root did not terminate"

    $watch = [Diagnostics.Stopwatch]::StartNew()
    while ($watch.Elapsed.TotalSeconds -lt 10 -and -not (Test-ProcessGone $agentPid)) {
        Start-Sleep -Milliseconds 100
    }
    Assert-Canary -Condition (Test-ProcessGone $agentPid) -Message "root termination left openab-agent.exe behind"
    Write-Host "[PASS] Windows Job Object removed the downstream agent on root termination"
    Write-Host "WINDOWS_DUAL_LAYER_CANARY_PASS"
}
finally {
    if ($null -ne $socket) {
        $socket.Dispose()
    }
    foreach ($process in @($agent, $root)) {
        if ($null -ne $process) {
            try {
                if (-not $process.HasExited) {
                    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
                    [void]$process.WaitForExit(5000)
                }
            }
            catch {
            }
            $process.Dispose()
        }
    }
    if ($null -ne $agentPid -and -not (Test-ProcessGone $agentPid)) {
        Stop-Process -Id $agentPid -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}
