param(
    [Parameter(Mandatory = $true)]
    [string] $ExePath,

    [string] $Session = "conpty-input-$([guid]::NewGuid().ToString('N'))",

    [string] $ReportPath = "",

    [string] $ExpectedConsoleHostPath = ""
)

$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param([string] $Command, [string[]] $Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $Command $($Arguments -join ' ')"
    }
}

function Read-Pane {
    param([string] $PaneId)
    $output = & $script:Exe pane read $PaneId --source recent-unwrapped --lines 200 --format text 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "pane read failed with exit code $LASTEXITCODE`: $($output -join "`n")"
    }
    return $output -join "`n"
}

function Wait-PaneText {
    param(
        [string] $PaneId,
        [string] $Needle,
        [int] $TimeoutSeconds = 10
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        $text = Read-Pane -PaneId $PaneId
        if ($text.Contains($Needle)) {
            return $text
        }
        Start-Sleep -Milliseconds 200
    } while ((Get-Date) -lt $deadline)
    return $text
}

function New-ProbePane {
    param([string] $Mode)
    $created = & $script:Exe workspace create --cwd $PWD.Path 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "workspace create failed with exit code $LASTEXITCODE`: $($created -join "`n")"
    }
    $paneId = (($created -join "`n") | ConvertFrom-Json).result.root_pane.pane_id
    if ([string]::IsNullOrWhiteSpace($paneId)) {
        throw "workspace create did not return a root pane id: $($created -join "`n")"
    }

    $command = "& '$script:ProbeExe' $Mode"
    Invoke-Checked $script:Exe @("pane", "run", $paneId, $command)
    $ready = "PROBE_READY_$($Mode.ToUpperInvariant())"
    $text = Wait-PaneText -PaneId $paneId -Needle $ready
    if (-not $text.Contains($ready)) {
        throw "$Mode probe did not become ready: $text"
    }
    return $paneId
}

function Get-LatestProbeHex {
    param([string] $PaneText)
    $matches = [regex]::Matches($PaneText, "PROBE_ALL:([0-9a-f]+)")
    if ($matches.Count -eq 0) {
        return ""
    }
    return $matches[$matches.Count - 1].Groups[1].Value
}

function Wait-PaneHexAppend {
    param(
        [string] $PaneId,
        [string] $PreviousHex,
        [string] $ExpectedHex,
        [int] $TimeoutSeconds = 4
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        $paneText = Read-Pane -PaneId $PaneId
        $latestHex = Get-LatestProbeHex -PaneText $paneText
        if ($latestHex.StartsWith($PreviousHex)) {
            $appendedHex = $latestHex.Substring($PreviousHex.Length)
            if ($appendedHex -eq $ExpectedHex) {
                return [ordered]@{ delivered = $true; pane = $paneText }
            }
        }
        Start-Sleep -Milliseconds 200
    } while ((Get-Date) -lt $deadline)
    return [ordered]@{ delivered = $false; pane = $paneText }
}

function Send-KeyAndObserve {
    param(
        [string] $PaneId,
        [string] $Key,
        [string] $ExpectedHex
    )
    $before = Get-LatestProbeHex -PaneText (Read-Pane -PaneId $PaneId)
    Invoke-Checked $script:Exe @("pane", "send-keys", $PaneId, $Key)
    $observed = Wait-PaneHexAppend -PaneId $PaneId -PreviousHex $before -ExpectedHex $ExpectedHex
    return [ordered]@{
        key = $Key
        expected_hex = $ExpectedHex
        delivered = $observed.delivered
        pane = $observed.pane
    }
}

function Send-RawAndObserve {
    param(
        [string] $PaneId,
        [string] $Text,
        [string] $ExpectedHex
    )
    $before = Get-LatestProbeHex -PaneText (Read-Pane -PaneId $PaneId)
    & $script:Exe pane send-text $PaneId $Text | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "pane send-text failed with exit code $LASTEXITCODE"
    }
    $observed = Wait-PaneHexAppend -PaneId $PaneId -PreviousHex $before -ExpectedHex $ExpectedHex
    return [ordered]@{
        expected_hex = $ExpectedHex
        delivered = $observed.delivered
        pane = $observed.pane
    }
}

$script:Exe = (Resolve-Path $ExePath).Path
$workDir = Join-Path ([System.IO.Path]::GetTempPath()) "herdr-conpty-input-$([guid]::NewGuid().ToString('N'))"
$probeSource = Join-Path $workDir "probe.rs"
$script:ProbeExe = Join-Path $workDir "probe.exe"
$oldSession = $env:HERDR_SESSION
$oldSocket = $env:HERDR_SOCKET_PATH
$oldClientSocket = $env:HERDR_CLIENT_SOCKET_PATH
$server = $null
$report = [ordered]@{}
$failed = $false

try {
    New-Item -ItemType Directory -Force -Path $workDir | Out-Null
@'
use std::ffi::c_void;

const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
const ENABLE_LINE_INPUT: u32 = 0x0002;
const ENABLE_ECHO_INPUT: u32 = 0x0004;
const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

type Handle = *mut c_void;

#[link(name = "Kernel32")]
extern "system" {
    fn GetStdHandle(kind: u32) -> Handle;
    fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: Handle, mode: u32) -> i32;
    fn ReadFile(
        handle: Handle,
        buffer: *mut c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn WriteFile(
        handle: Handle,
        buffer: *const c_void,
        bytes_to_write: u32,
        bytes_written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

fn write_all(handle: Handle, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let mut written = 0;
        let ok = unsafe {
            WriteFile(
                handle,
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || written == 0 {
            std::process::exit(3);
        }
        bytes = &bytes[written as usize..];
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "legacy".to_string());
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let mut console_mode = 0;
    if unsafe { GetConsoleMode(input, &mut console_mode) } == 0 {
        write_all(output, b"PROBE_ERROR_GET_MODE\r\n");
        std::process::exit(1);
    }
    let raw_vt_mode = (console_mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
        & !(ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT);
    if unsafe { SetConsoleMode(input, raw_vt_mode) } == 0 {
        write_all(output, b"PROBE_ERROR_SET_MODE\r\n");
        std::process::exit(2);
    }

    if mode == "kitty" {
        write_all(output, b"\x1b[>7u\x1b[?u\x1b[cPROBE_READY_KITTY\r\n");
    } else {
        write_all(output, b"PROBE_READY_LEGACY\r\n");
    }

    let mut all = Vec::new();
    let mut buffer = [0u8; 256];
    loop {
        let mut read = 0;
        let ok = unsafe {
            ReadFile(
                input,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            break;
        }
        all.extend_from_slice(&buffer[..read as usize]);
        let mut line = String::from("PROBE_ALL:");
        for byte in &all {
            use std::fmt::Write as _;
            let _ = write!(&mut line, "{byte:02x}");
        }
        line.push_str("\r\n");
        write_all(output, line.as_bytes());
    }
}
'@ | Set-Content -NoNewline -Encoding utf8 $probeSource

    Invoke-Checked rustc @("--edition", "2021", $probeSource, "-o", $script:ProbeExe)

    $env:HERDR_SESSION = $Session
    Remove-Item Env:HERDR_SOCKET_PATH -ErrorAction SilentlyContinue
    Remove-Item Env:HERDR_CLIENT_SOCKET_PATH -ErrorAction SilentlyContinue

    $os = Get-CimInstance Win32_OperatingSystem
    $report.os = [ordered]@{
        caption = $os.Caption
        version = $os.Version
        build = $os.BuildNumber
    }

    Invoke-Checked $script:Exe @("--version")
    & $script:Exe --default-config | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $script:Exe --default-config"
    }

    $server = Start-Process -FilePath $script:Exe -ArgumentList "server" -PassThru -WindowStyle Hidden
    $deadline = (Get-Date).AddSeconds(10)
    $serverReady = $false
    do {
        Start-Sleep -Milliseconds 250
        $status = & $script:Exe status server 2>&1
        if ($LASTEXITCODE -eq 0 -and (($status -join "`n") -match "status: running")) {
            $serverReady = $true
            break
        }
    } while ((Get-Date) -lt $deadline)
    if (-not $serverReady) {
        throw "server did not become ready: $($status -join "`n")"
    }

    $legacyPane = New-ProbePane -Mode "legacy"
    $report.legacy_alt_v = Send-KeyAndObserve -PaneId $legacyPane -Key "alt+v" -ExpectedHex "1b76"

    $kittyPane = New-ProbePane -Mode "kitty"
    $report.kitty_initial = Wait-PaneText -PaneId $kittyPane -Needle "1b5b3f3775"
    $kittyInitialHex = Get-LatestProbeHex -PaneText $report.kitty_initial
    $report.device_attributes_response = $kittyInitialHex -match "1b5b3f(?:3[0-9]|3b)+63"
    $report.kitty_query_response = $kittyInitialHex.Contains("1b5b3f3775")
    $report.kitty_alt_v = Send-KeyAndObserve -PaneId $kittyPane -Key "alt+v" -ExpectedHex "1b5b3131383b333a3175"
    $report.kitty_ctrl_u = Send-KeyAndObserve -PaneId $kittyPane -Key "ctrl+u" -ExpectedHex "1b5b3131373b353a3175"
    $report.kitty_up = Send-KeyAndObserve -PaneId $kittyPane -Key "up" -ExpectedHex "1b5b313b313a3141"
    $report.kitty_escape = Send-KeyAndObserve -PaneId $kittyPane -Key "esc" -ExpectedHex "1b5b323775"
    $report.raw_kitty_alt_v = Send-RawAndObserve -PaneId $kittyPane -Text ([char]27 + "[118;3:1u") -ExpectedHex "1b5b3131383b333a3175"
    $report.raw_kitty_ctrl_u = Send-RawAndObserve -PaneId $kittyPane -Text ([char]27 + "[117;5:1u") -ExpectedHex "1b5b3131373b353a3175"
    $report.raw_alt_v = Send-RawAndObserve -PaneId $kittyPane -Text ([char]27 + "v") -ExpectedHex "1b76"
    $report.raw_ctrl_u = Send-RawAndObserve -PaneId $kittyPane -Text ([string][char]0x15) -ExpectedHex "15"

    $consoleHosts = @(Get-Process -Name conhost, OpenConsole -ErrorAction SilentlyContinue | ForEach-Object {
        $path = ""
        $version = ""
        try { $path = $_.Path } catch {}
        try { $version = $_.MainModule.FileVersionInfo.FileVersion } catch {}
        [ordered]@{
            name = $_.ProcessName
            path = $path
            version = $version
        }
    })
    $report.runner_console_hosts = $consoleHosts
    $appLocalHostRequired = -not [string]::IsNullOrWhiteSpace($ExpectedConsoleHostPath)
    if ($appLocalHostRequired) {
        $expectedConsoleHost = (Resolve-Path $ExpectedConsoleHostPath).Path
        $report.expected_console_host = $expectedConsoleHost
        $report.app_local_console_host = @($consoleHosts | Where-Object {
            $_.name -ieq "OpenConsole" -and $_.path -ieq $expectedConsoleHost
        }).Count -gt 0
    } else {
        $report.app_local_console_host = $null
    }

    $failed = -not $report.legacy_alt_v.delivered `
        -or -not $report.device_attributes_response `
        -or -not $report.kitty_query_response `
        -or -not $report.kitty_alt_v.delivered `
        -or -not $report.kitty_ctrl_u.delivered `
        -or -not $report.kitty_up.delivered `
        -or -not $report.kitty_escape.delivered `
        -or -not $report.raw_kitty_alt_v.delivered `
        -or -not $report.raw_kitty_ctrl_u.delivered `
        -or -not $report.raw_alt_v.delivered `
        -or -not $report.raw_ctrl_u.delivered `
        -or ($appLocalHostRequired -and -not $report.app_local_console_host)
} finally {
    if ($null -ne $server) {
        try {
            $stopOutput = & $script:Exe server stop 2>&1
            if ($LASTEXITCODE -ne 0) {
                Write-Host "server stop during cleanup failed: $($stopOutput -join "`n")"
            }
        } catch {
            Write-Host "server stop during cleanup failed: $($_.Exception.Message)"
        }
        Wait-Process -Id $server.Id -Timeout 10 -ErrorAction SilentlyContinue
        $server.Refresh()
        if (-not $server.HasExited) {
            & taskkill.exe /PID $server.Id /T /F 2>&1 | Out-Null
        }
    }
    $global:LASTEXITCODE = 0
    if ($null -eq $oldSession) {
        Remove-Item Env:HERDR_SESSION -ErrorAction SilentlyContinue
    } else {
        $env:HERDR_SESSION = $oldSession
    }
    if ($null -eq $oldSocket) {
        Remove-Item Env:HERDR_SOCKET_PATH -ErrorAction SilentlyContinue
    } else {
        $env:HERDR_SOCKET_PATH = $oldSocket
    }
    if ($null -eq $oldClientSocket) {
        Remove-Item Env:HERDR_CLIENT_SOCKET_PATH -ErrorAction SilentlyContinue
    } else {
        $env:HERDR_CLIENT_SOCKET_PATH = $oldClientSocket
    }

    $json = $report | ConvertTo-Json -Depth 8
    Write-Host $json
    if (-not [string]::IsNullOrWhiteSpace($ReportPath)) {
        $reportParent = Split-Path -Parent $ReportPath
        if (-not [string]::IsNullOrWhiteSpace($reportParent)) {
            New-Item -ItemType Directory -Force -Path $reportParent | Out-Null
        }
        $json | Set-Content -Encoding utf8 $ReportPath
    }
    Remove-Item -Recurse -Force $workDir -ErrorAction SilentlyContinue
}

if ($failed) {
    throw "enhanced Windows ConPTY input probe failed; see the JSON report above"
}
