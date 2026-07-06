# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=codebuddy
# HERDR_INTEGRATION_VERSION=3

param([string]$Action = "")

if ($Action -notin @("session", "working", "blocked", "idle")) { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}

$hookEventName = "$($payload.hook_event_name)"
$sessionId = $payload.session_id
$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()

function Send-HerdrRequest {
    param(
        [string]$Method,
        [hashtable]$Params
    )
    try {
        $args = @(
            "pane",
            $Method,
            $env:HERDR_PANE_ID
        )
        foreach ($key in @("source", "agent", "state", "seq", "agent-session-id", "session-start-source")) {
            if ($Params.ContainsKey($key)) {
                $args += @("--$key", "$($Params[$key])")
            }
        }
        & herdr @args 2>$null | Out-Null
    } catch {
    }
}

if ($Action -eq "session") {
    if ($hookEventName -and $hookEventName -ne "SessionStart") { exit 0 }
    if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }
    $sessionParams = @{
        source = "herdr:codebuddy"
        agent = "codebuddy"
        seq = $seq
        "agent-session-id" = $sessionId
    }
    if ($hookEventName -eq "SessionStart" -and $payload.source -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.source)) {
        $sessionParams["session-start-source"] = "$($payload.source)"
    }
    Send-HerdrRequest -Method "report-agent-session" -Params $sessionParams
    # Also report idle state so Herdr treats the hook as the lifecycle
    # authority immediately instead of falling back to screen detection.
    $idleParams = @{
        source = "herdr:codebuddy"
        agent = "codebuddy"
        state = "idle"
        seq = $seq + 1
        "agent-session-id" = $sessionId
    }
    Send-HerdrRequest -Method "report-agent" -Params $idleParams
} else {
    $stateParams = @{
        source = "herdr:codebuddy"
        agent = "codebuddy"
        state = $Action
        seq = $seq
    }
    if (-not [string]::IsNullOrWhiteSpace($sessionId)) {
        $stateParams["agent-session-id"] = $sessionId
    }
    Send-HerdrRequest -Method "report-agent" -Params $stateParams
}
