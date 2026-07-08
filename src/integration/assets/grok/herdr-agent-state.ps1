# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=grok
# HERDR_INTEGRATION_VERSION=1

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

# Grok Build hook payloads use camelCase keys; GROK_SESSION_ID is a
# runner-injected fallback set on every hook process.
$sessionId = if ($null -ne $payload) { $payload.sessionId } else { $null }
if ([string]::IsNullOrWhiteSpace($sessionId)) { $sessionId = $env:GROK_SESSION_ID }
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
try {
    & herdr pane report-agent-session $env:HERDR_PANE_ID --source herdr:grok --agent grok --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
} catch {
}
