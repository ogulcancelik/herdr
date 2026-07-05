param(
    [Parameter(Mandatory = $true)]
    [string] $ExePath,

    [string] $Session = "ci-windows-$([guid]::NewGuid().ToString('N'))"
)

$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param([string] $Command, [string[]] $Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $Command $($Arguments -join ' ')"
    }
}

$exe = (Resolve-Path $ExePath).Path
$fakeDir = Join-Path ([System.IO.Path]::GetTempPath()) "herdr-fake-conpty-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force $fakeDir | Out-Null

$fakeSource = Join-Path $fakeDir "fake_conpty.rs"
$fakeDll = Join-Path $fakeDir "conpty.dll"
@'
#![allow(non_snake_case)]

use std::ffi::c_void;

#[repr(C)]
pub struct COORD {
    pub X: i16,
    pub Y: i16,
}

type HANDLE = *mut c_void;
type HRESULT = i32;

#[no_mangle]
pub extern "system" fn CreatePseudoConsole(
    _size: COORD,
    _h_input: HANDLE,
    _h_output: HANDLE,
    _flags: u32,
    _hpc: *mut HANDLE,
) -> HRESULT {
    -2147467259
}

#[no_mangle]
pub extern "system" fn ResizePseudoConsole(_hpc: HANDLE, _size: COORD) -> HRESULT {
    -2147467259
}

#[no_mangle]
pub extern "system" fn ClosePseudoConsole(_hpc: HANDLE) {}
'@ | Set-Content -NoNewline -Encoding utf8 $fakeSource

Invoke-Checked rustc @("--crate-type", "cdylib", "--edition", "2021", $fakeSource, "-o", $fakeDll)

$oldPath = $env:PATH
$oldSession = $env:HERDR_SESSION
$env:PATH = "$fakeDir;$oldPath"
$env:HERDR_SESSION = $Session

$server = $null
try {
    Invoke-Checked $exe @("--version")
    & $exe --default-config | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit code $LASTEXITCODE`: $exe --default-config"
    }

    $server = Start-Process -FilePath $exe -ArgumentList "server" -PassThru -WindowStyle Hidden
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 250
        $status = & $exe status server 2>&1
        if ($LASTEXITCODE -eq 0 -and (($status -join "`n") -match "status: running")) {
            break
        }
    } while ((Get-Date) -lt $deadline)

    if ((Get-Date) -ge $deadline) {
        throw "server did not become ready"
    }

    Invoke-Checked $exe @("agent", "start", "smoke", "--", $env:ComSpec, "/K", "dir")
    $text = ""
    $deadline = (Get-Date).AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 500
        try {
            $read = & $exe agent read smoke --source recent --lines 40 --format text 2>&1
            $readExitCode = $LASTEXITCODE
        } catch {
            $read = @($_.Exception.Message)
            $readExitCode = 1
        }
        $text = $read -join "`n"
        if ($readExitCode -eq 0 -and $text -match "File\(s\)" -and $text -match "Dir\(s\)") {
            break
        }
    } while ((Get-Date) -lt $deadline)

    if ($text -notmatch "File\(s\)" -or $text -notmatch "Dir\(s\)") {
        throw "pane read did not include cmd.exe directory output: $text"
    }
} finally {
    if ($null -ne $server) {
        try {
            $stopOutput = & $exe server stop 2>&1
            if ($LASTEXITCODE -ne 0) {
                Write-Host "server stop during cleanup exited with $LASTEXITCODE`: $($stopOutput -join "`n")"
            }
        } catch {
            Write-Host "server stop during cleanup failed: $($_.Exception.Message)"
        }
        Wait-Process -Id $server.Id -Timeout 10 -ErrorAction SilentlyContinue
    }
    $global:LASTEXITCODE = 0
    $env:PATH = $oldPath
    $env:HERDR_SESSION = $oldSession
    Remove-Item -Recurse -Force $fakeDir -ErrorAction SilentlyContinue
}
