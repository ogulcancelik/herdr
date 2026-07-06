# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=commandcode
# HERDR_INTEGRATION_VERSION=1

param([string]$Action = "")

if ($Action -ne "working" -and $Action -ne "idle") {
    exit 0
}

if ($env:HERDR_ENV -ne "1" -or -not $env:HERDR_SOCKET_PATH -or -not $env:HERDR_PANE_ID) {
    exit 0
}

$hookInput = @{}
try {
    $stdin = [Console]::In.ReadToEnd()
    if ($stdin.Trim().Length -gt 0) {
        $hookInput = $stdin | ConvertFrom-Json
    }
} catch {
    $hookInput = @{}
}

$sessionId = $hookInput.session_id
if (-not $sessionId) {
    $sessionId = $env:COMMANDCODE_SESSION_ID
}

$params = @{
    pane_id = $env:HERDR_PANE_ID
    source = "herdr:commandcode"
    agent = "commandcode"
    seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    state = $Action
}
if ($sessionId) {
    $params.agent_session_id = $sessionId
}

$request = @{
    id = "herdr:commandcode:$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
    method = "pane.report_agent"
    params = $params
} | ConvertTo-Json -Compress

try {
    $client = [System.Net.Sockets.UnixDomainSocketEndPoint]::new($env:HERDR_SOCKET_PATH)
    $socket = [System.Net.Sockets.Socket]::new(
        [System.Net.Sockets.AddressFamily]::Unix,
        [System.Net.Sockets.SocketType]::Stream,
        [System.Net.Sockets.ProtocolType]::Unspecified
    )
    $socket.Connect($client)
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($request + "`n")
    [void]$socket.Send($bytes)
    $socket.Close()
} catch {
}
