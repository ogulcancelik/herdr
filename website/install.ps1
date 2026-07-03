[CmdletBinding()]
param(
    [string]$Channel = $env:HERDR_CHANNEL,
    [string]$ManifestUrl = $env:HERDR_MANIFEST_URL,
    [string]$InstallDir = $env:HERDR_INSTALL_DIR,
    [int]$Retain = 3
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ([string]::IsNullOrWhiteSpace($Channel)) {
    $Channel = "preview"
}

if ($Channel -notin @("stable", "preview")) {
    Write-Error "Invalid Herdr channel '$Channel'. Use 'preview'."
    exit 1
}

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message"
}

function Write-WarningStep {
    param([string]$Message)
    Write-Warning $Message
}

function Get-HerdrCommandSource {
    $existing = Get-Command herdr -ErrorAction SilentlyContinue
    if ($null -eq $existing) {
        return $null
    }

    return $existing.Source
}

function Test-PathStartsWith {
    param(
        [string]$Path,
        [string]$Prefix
    )

    if ([string]::IsNullOrWhiteSpace($Path) -or [string]::IsNullOrWhiteSpace($Prefix)) {
        return $false
    }

    try {
        $normalizedPath = [System.IO.Path]::GetFullPath($Path)
        $normalizedPrefix = [System.IO.Path]::GetFullPath($Prefix).TrimEnd("\") + "\"
        return $normalizedPath.StartsWith($normalizedPrefix, [System.StringComparison]::OrdinalIgnoreCase)
    } catch {
        return $false
    }
}

function Path-Contains {
    param(
        [string]$PathValue,
        [string]$Entry
    )

    if ([string]::IsNullOrWhiteSpace($PathValue)) {
        return $false
    }

    $needle = $Entry.TrimEnd("\")
    foreach ($segment in $PathValue.Split(";", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        if ($segment.TrimEnd("\") -ieq $needle) {
            return $true
        }
    }

    return $false
}

function Prepend-PathEntry {
    param(
        [string]$PathValue,
        [string]$Entry
    )

    $needle = $Entry.TrimEnd("\")
    $segments = @($Entry)
    if (-not [string]::IsNullOrWhiteSpace($PathValue)) {
        $segments += $PathValue.Split(";", [System.StringSplitOptions]::RemoveEmptyEntries) |
            Where-Object { $_.TrimEnd("\") -ine $needle }
    }

    return ($segments -join ";")
}

function Get-ManifestAsset {
    param(
        [object]$Manifest,
        [string]$Target
    )

    $property = $Manifest.assets.PSObject.Properties[$Target]
    if ($null -eq $property) {
        throw "Release manifest does not include a binary for $Target."
    }

    $asset = $property.Value
    if ($asset -is [string]) {
        return [PSCustomObject]@{
            Url = $asset
            Sha256 = $null
        }
    }

    if ([string]::IsNullOrWhiteSpace([string]$asset.url)) {
        throw "Release manifest asset $Target is missing a URL."
    }

    return [PSCustomObject]@{
        Url = [string]$asset.url
        Sha256 = if ([string]::IsNullOrWhiteSpace([string]$asset.sha256)) { $null } else { [string]$asset.sha256 }
    }
}

function ConvertTo-ManifestObject {
    param([object]$Manifest)

    if ($Manifest -isnot [string]) {
        return $Manifest
    }

    $json = $Manifest.TrimStart([char]0xFEFF)
    if ($json.StartsWith("ï»¿")) {
        $json = $json.Substring(3)
    }

    return $json | ConvertFrom-Json
}

function Test-FileDigest {
    param(
        [string]$Path,
        [string]$ExpectedDigest
    )

    if ([string]::IsNullOrWhiteSpace($ExpectedDigest)) {
        return
    }

    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.IO.File]::ReadAllBytes($Path)
        $actual = [System.BitConverter]::ToString($sha256.ComputeHash($bytes)).Replace("-", "").ToLowerInvariant()
    } finally {
        $sha256.Dispose()
    }
    if ($actual -ne $ExpectedDigest.ToLowerInvariant()) {
        throw "Downloaded Herdr checksum did not match. Expected $ExpectedDigest but got $actual."
    }
}

function Invoke-WithInstallLock {
    param(
        [string]$LockPath,
        [scriptblock]$Script
    )

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $LockPath) | Out-Null
    $lock = $null
    while ($null -eq $lock) {
        try {
            $lock = [System.IO.File]::Open(
                $LockPath,
                [System.IO.FileMode]::OpenOrCreate,
                [System.IO.FileAccess]::ReadWrite,
                [System.IO.FileShare]::None
            )
        } catch [System.IO.IOException] {
            Start-Sleep -Milliseconds 250
        }
    }

    try {
        & $Script
    } finally {
        $lock.Dispose()
    }
}

function Test-IsJunction {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return $false
    }

    $item = Get-Item -LiteralPath $Path -Force
    return ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -and $item.LinkType -eq "Junction"
}

function Set-ManagedJunction {
    param(
        [string]$LinkPath,
        [string]$TargetPath,
        [string]$ManagedTargetPrefix,
        [bool]$AllowLegacyHerdrBinMigration = $false
    )

    if (Test-Path -LiteralPath $LinkPath) {
        $item = Get-Item -LiteralPath $LinkPath -Force
        if (Test-IsJunction -Path $LinkPath) {
            $existingTarget = [string]$item.Target
            if (-not [string]::IsNullOrWhiteSpace($ManagedTargetPrefix)) {
                $ownedPrefix = $ManagedTargetPrefix.TrimEnd("\")
                if (-not $existingTarget.StartsWith($ownedPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                    throw "Refusing to retarget junction at $LinkPath because it is not managed by this installer."
                }
            }
            if ($existingTarget.Equals($TargetPath, [System.StringComparison]::OrdinalIgnoreCase)) {
                return
            }
            Remove-Item -LiteralPath $LinkPath -Recurse -Force
        } elseif ($item.PSIsContainer) {
            if ((Get-ChildItem -LiteralPath $LinkPath -Force | Select-Object -First 1) -ne $null) {
                if (-not (Move-LegacyHerdrBinDirectory -Path $LinkPath -AllowMigration $AllowLegacyHerdrBinMigration)) {
                    throw "Refusing to replace non-empty directory at $LinkPath with a junction."
                }
            } else {
                Remove-Item -LiteralPath $LinkPath -Recurse -Force
            }
        } else {
            throw "Refusing to replace file at $LinkPath with a junction."
        }
    }

    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $LinkPath) | Out-Null
    New-Item -ItemType Junction -Path $LinkPath -Target $TargetPath | Out-Null
}

function Move-LegacyHerdrBinDirectory {
    param(
        [string]$Path,
        [bool]$AllowMigration
    )

    if (-not $AllowMigration) {
        return $false
    }

    $entries = @(Get-ChildItem -LiteralPath $Path -Force)
    if (($entries | Where-Object { $_.PSIsContainer } | Select-Object -First 1) -ne $null) {
        return $false
    }

    if (($entries | Where-Object { $_.Name -ieq "herdr.exe" } | Select-Object -First 1) -eq $null) {
        return $false
    }

    $legacyPath = "$Path.legacy.$([System.Guid]::NewGuid().ToString("N"))"
    Move-Item -LiteralPath $Path -Destination $legacyPath
    Write-Step "Moved legacy Herdr bin directory to $legacyPath."
    return $true
}

function Remove-StaleInstallArtifacts {
    param([string]$ReleasesDir)

    if (-not (Test-Path -LiteralPath $ReleasesDir -PathType Container)) {
        return
    }

    Get-ChildItem -LiteralPath $ReleasesDir -Force -Directory -Filter ".staging.*" -ErrorAction SilentlyContinue |
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
}

function Remove-OldReleases {
    param(
        [string]$ReleasesDir,
        [string]$CurrentReleaseDir,
        [int]$Keep
    )

    if ($Keep -lt 1 -or -not (Test-Path -LiteralPath $ReleasesDir -PathType Container)) {
        return
    }

    $currentFullPath = [System.IO.Path]::GetFullPath($CurrentReleaseDir)
    $releaseDirs = Get-ChildItem -LiteralPath $ReleasesDir -Force -Directory -ErrorAction SilentlyContinue |
        Where-Object { -not $_.Name.StartsWith(".staging.") } |
        Sort-Object LastWriteTimeUtc -Descending
    $kept = 0
    foreach ($dir in $releaseDirs) {
        $dirFullPath = [System.IO.Path]::GetFullPath($dir.FullName)
        if ($dirFullPath.Equals($currentFullPath, [System.StringComparison]::OrdinalIgnoreCase)) {
            $kept += 1
            continue
        }
        if ($kept -lt $Keep) {
            $kept += 1
            continue
        }
        Remove-Item -LiteralPath $dir.FullName -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Resolve-HerdrVersion {
    param(
        [object]$Manifest,
        [string]$SelectedChannel
    )

    if ($SelectedChannel -eq "preview") {
        if ([string]::IsNullOrWhiteSpace([string]$Manifest.base_version) -or [string]::IsNullOrWhiteSpace([string]$Manifest.build_id)) {
            throw "Preview manifest is missing base_version or build_id."
        }
        return "$($Manifest.base_version)-preview.$($Manifest.build_id)"
    }

    if ([string]::IsNullOrWhiteSpace([string]$Manifest.version)) {
        throw "Stable manifest is missing version."
    }
    return [string]$Manifest.version
}

function Get-HerdrConfigPath {
    if (-not [string]::IsNullOrWhiteSpace($env:HERDR_CONFIG_PATH)) {
        return $env:HERDR_CONFIG_PATH
    }

    $appDir = "herdr"
    if (-not [string]::IsNullOrWhiteSpace($env:XDG_CONFIG_HOME)) {
        return Join-Path (Join-Path $env:XDG_CONFIG_HOME $appDir) "config.toml"
    }
    if (-not [string]::IsNullOrWhiteSpace($env:APPDATA)) {
        return Join-Path (Join-Path $env:APPDATA $appDir) "config.toml"
    }
    if (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        return Join-Path (Join-Path (Join-Path (Join-Path $env:USERPROFILE "AppData") "Roaming") $appDir) "config.toml"
    }
    if (-not [string]::IsNullOrWhiteSpace($env:HOME)) {
        return Join-Path (Join-Path (Join-Path $env:HOME ".config") $appDir) "config.toml"
    }

    return Join-Path (Join-Path ([System.IO.Path]::GetTempPath()) $appDir) "config.toml"
}

function Set-HerdrUpdateChannelConfig {
    param([string]$SelectedChannel)

    $configPath = Get-HerdrConfigPath
    $configDir = Split-Path -Parent $configPath
    if (-not [string]::IsNullOrWhiteSpace($configDir)) {
        New-Item -ItemType Directory -Force -Path $configDir | Out-Null
    }

    $content = if (Test-Path -LiteralPath $configPath -PathType Leaf) {
        Get-Content -LiteralPath $configPath -Raw
    } else {
        ""
    }

    $valueLine = "channel = `"$SelectedChannel`""
    $match = [regex]::Match($content, "(?ms)^(\s*\[update\]\s*)(.*?)(?=^\s*\[|\z)")
    if ($match.Success) {
        $body = $match.Groups[2].Value
        if ($body -match "(?m)^\s*channel\s*=") {
            $updatedBody = [regex]::Replace($body, "(?m)^\s*channel\s*=.*$", $valueLine, 1)
        } else {
            $updatedBody = $body.TrimEnd() + "`r`n$valueLine`r`n"
        }
        $bodyStart = $match.Groups[2].Index
        $updated = $content.Substring(0, $bodyStart) + $updatedBody + $content.Substring($bodyStart + $body.Length)
    } elseif ([string]::IsNullOrWhiteSpace($content)) {
        $updated = "[update]`r`n$valueLine`r`n"
    } else {
        $updated = $content.TrimEnd() + "`r`n`r`n[update]`r`n$valueLine`r`n"
    }

    Set-Content -LiteralPath $configPath -Value $updated -NoNewline
    Write-Step "Herdr update channel set to $SelectedChannel in $configPath."
}

if ($env:OS -ne "Windows_NT") {
    Write-Error "install.ps1 supports Windows only. Use install.sh on Linux or macOS."
    exit 1
}

if (-not [Environment]::Is64BitOperatingSystem) {
    Write-Error "Herdr requires 64-bit Windows."
    exit 1
}

if ($Channel -eq "stable") {
    Write-Error "Windows builds are preview-only for now. Omit -Channel or use -Channel preview."
    exit 1
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation,mscorlib]::OSArchitecture.ToString()
switch ($architecture) {
    "X64" {
        $target = "windows-x86_64"
        $targetTriple = "x86_64-pc-windows-msvc"
    }
    "Arm64" {
        $target = "windows-x86_64"
        $targetTriple = "x86_64-pc-windows-msvc"
        Write-Step "Windows ARM64 detected; installing the x86_64 build under Windows emulation."
    }
    default {
        Write-Error "Unsupported Windows architecture: $architecture"
        exit 1
    }
}

if ([string]::IsNullOrWhiteSpace($ManifestUrl)) {
    $ManifestUrl = if ($Channel -eq "preview") {
        "https://herdr.dev/preview.json"
    } else {
        "https://herdr.dev/latest.json"
    }
}

$herdrHome = if ([string]::IsNullOrWhiteSpace($env:HERDR_HOME)) {
    Join-Path $env:USERPROFILE ".herdr"
} else {
    $env:HERDR_HOME
}
$standaloneRoot = Join-Path $herdrHome "packages\standalone"
$releasesDir = Join-Path $standaloneRoot "releases"
$currentDir = Join-Path $standaloneRoot "current"
$lockPath = Join-Path $standaloneRoot "install.lock"

$defaultVisibleBinDir = Join-Path $env:LOCALAPPDATA "Programs\Herdr\bin"
$visibleBinDir = if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $defaultVisibleBinDir
} else {
    $InstallDir
}
$allowLegacyVisibleBinMigration = $false
try {
    $allowLegacyVisibleBinMigration = [System.IO.Path]::GetFullPath($visibleBinDir).TrimEnd("\").Equals(
        [System.IO.Path]::GetFullPath($defaultVisibleBinDir).TrimEnd("\"),
        [System.StringComparison]::OrdinalIgnoreCase
    )
} catch {
    $allowLegacyVisibleBinMigration = $false
}

$existingHerdr = Get-HerdrCommandSource
if (-not [string]::IsNullOrWhiteSpace($existingHerdr) -and -not (Test-PathStartsWith -Path $existingHerdr -Prefix $visibleBinDir)) {
    Write-Step "Detected existing Herdr command at $existingHerdr"
    Write-WarningStep "PATH order decides which Herdr runs. This installer will put $visibleBinDir first for future and current PowerShell sessions."
}

Write-Step "Fetching Herdr $Channel manifest"
$manifest = ConvertTo-ManifestObject -Manifest (Invoke-RestMethod -Uri $ManifestUrl)
$versionIdentity = Resolve-HerdrVersion -Manifest $manifest -SelectedChannel $Channel
$asset = Get-ManifestAsset -Manifest $manifest -Target $target
$safeVersionIdentity = $versionIdentity -replace '[^0-9A-Za-z._-]', '-'
$releaseName = "$safeVersionIdentity-$targetTriple"
$releaseDir = Join-Path $releasesDir $releaseName

Write-Step "Installing Herdr $versionIdentity for $targetTriple"
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("herdr-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tempDir | Out-Null

try {
    Invoke-WithInstallLock -LockPath $lockPath -Script {
        Remove-StaleInstallArtifacts -ReleasesDir $releasesDir

        if (-not (Test-Path -LiteralPath (Join-Path $releaseDir "herdr.exe") -PathType Leaf)) {
            if (Test-Path -LiteralPath $releaseDir) {
                Remove-Item -LiteralPath $releaseDir -Recurse -Force
            }

            $downloadPath = Join-Path $tempDir "herdr.exe"
            $stagingDir = Join-Path $releasesDir ".staging.$releaseName.$PID"
            Write-Step "Downloading Herdr"
            Invoke-WebRequest -Uri $asset.Url -OutFile $downloadPath
            Test-FileDigest -Path $downloadPath -ExpectedDigest $asset.Sha256

            New-Item -ItemType Directory -Force -Path $stagingDir | Out-Null
            Copy-Item -LiteralPath $downloadPath -Destination (Join-Path $stagingDir "herdr.exe")
            Move-Item -LiteralPath $stagingDir -Destination $releaseDir
        }

        Set-ManagedJunction -LinkPath $currentDir -TargetPath $releaseDir -ManagedTargetPrefix $releasesDir
        Set-ManagedJunction -LinkPath $visibleBinDir -TargetPath $releaseDir -ManagedTargetPrefix $standaloneRoot -AllowLegacyHerdrBinMigration $allowLegacyVisibleBinMigration

        $herdrCommand = Join-Path $visibleBinDir "herdr.exe"
        & $herdrCommand --version *> $null
        if ($LASTEXITCODE -ne 0) {
            throw "Installed Herdr command failed verification: $herdrCommand --version"
        }

        Remove-OldReleases -ReleasesDir $releasesDir -CurrentReleaseDir $releaseDir -Keep $Retain
    }
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}

Set-HerdrUpdateChannelConfig -SelectedChannel $Channel

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$newUserPath = Prepend-PathEntry -PathValue $userPath -Entry $visibleBinDir
if ($newUserPath -cne $userPath) {
    [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    Write-Step "PATH updated for future PowerShell sessions."
} else {
    Write-Step "$visibleBinDir is already first on PATH."
}

$newProcessPath = Prepend-PathEntry -PathValue $env:Path -Entry $visibleBinDir
if ($newProcessPath -cne $env:Path) {
    $env:Path = $newProcessPath
}

$resolvedHerdr = Get-HerdrCommandSource
if (-not (Test-PathStartsWith -Path $resolvedHerdr -Prefix $visibleBinDir)) {
    Write-WarningStep "PowerShell still resolves herdr to $resolvedHerdr. Open a new PowerShell window or inspect PATH order manually."
}

Write-Step "Current PowerShell session: herdr"
Write-Step "Future PowerShell windows: open a new PowerShell window and run: herdr"
Write-Host "Herdr $versionIdentity installed successfully."
