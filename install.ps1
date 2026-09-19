#requires -version 5.1
<#
.SYNOPSIS
    Installs fzv from its GitHub releases.

.DESCRIPTION
    One line, from PowerShell:

        irm https://github.com/ref42/fzv/releases/latest/download/install.ps1 | iex

    The archive for this machine's architecture is downloaded, checked against
    the SHA-256 published in the release's SHA256SUMS, unpacked, and the install
    directory is added to the user PATH. fzv installs Zig itself, so the next
    step is the one it prints at the end:

        fzv get dev, stable -path D:\zig

.PARAMETER BaseUrl
    Where the release assets are fetched from. Point it at a mirror or an
    internal fork to install from somewhere else.

.PARAMETER InstallDir
    Where fzv.exe is installed. Defaults to %LOCALAPPDATA%\Programs\fzv, which
    needs no administrator rights.
#>
[CmdletBinding()]
param(
    [string]$BaseUrl = 'https://github.com/ref42/fzv/releases/latest/download',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\fzv')
)

$ErrorActionPreference = 'Stop'
# A progress bar makes Invoke-WebRequest an order of magnitude slower.
$ProgressPreference = 'SilentlyContinue'

$releaseBase = $BaseUrl.TrimEnd('/')
$registryPath = 'HKCU:\Environment'

function Get-FzvArchitecture {
    # A 32-bit shell on a 64-bit machine reports x86 in PROCESSOR_ARCHITECTURE
    # and the real one in PROCESSOR_ARCHITEW6432.
    $arch = if ($env:PROCESSOR_ARCHITEW6432) {
        $env:PROCESSOR_ARCHITEW6432
    } else {
        $env:PROCESSOR_ARCHITECTURE
    }
    switch ($arch) {
        'AMD64' { 'x86_64' }
        'ARM64' { 'aarch64' }
        default { throw "fzv is published for x86_64 and aarch64, not for '$arch'" }
    }
}

function Get-RemoteFile {
    param(
        [Parameter(Mandatory)][string]$Uri,
        [Parameter(Mandatory)][string]$OutFile
    )

    try {
        Invoke-WebRequest -Uri $Uri -OutFile $OutFile -Headers @{ 'User-Agent' = 'fzv-install' }
    } catch {
        # The transport error alone does not say which URL was missing.
        throw "unable to download $Uri ($($_.Exception.Message))"
    }
}

# The SHA-256 the release publishes for one asset, read out of SHA256SUMS.
function Get-PublishedHash {
    param(
        [Parameter(Mandatory)][string]$SumsPath,
        [Parameter(Mandatory)][string]$Name
    )

    foreach ($line in Get-Content -Path $SumsPath) {
        # `<hash>  <name>`, the way sha256sum writes it; a `*` marks binary mode.
        $fields = $line -split '\s+'
        if ($fields.Count -ge 2 -and $fields[1].TrimStart('*') -eq $Name) {
            return $fields[0].ToLowerInvariant()
        }
    }
    throw "$Name is not listed in SHA256SUMS"
}

# Tells running programs that the environment changed, so that a terminal
# started afterwards inherits the new PATH without a re-login. Best effort: an
# install that has already happened must not fail because of it.
function Send-EnvironmentChange {
    try {
        if (-not ('FzvInstall.NativeMethods' -as [type])) {
            Add-Type -Namespace FzvInstall -Name NativeMethods -MemberDefinition @'
[DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, IntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out IntPtr lpdwResult);
'@
        }
        $result = [IntPtr]::Zero
        [FzvInstall.NativeMethods]::SendMessageTimeout(
            [IntPtr]0xffff, 0x001A, [IntPtr]::Zero, 'Environment', 2, 5000, [ref]$result
        ) | Out-Null
    } catch {
        Write-Verbose "the environment change could not be broadcast: $($_.Exception.Message)"
    }
}

# Adds `PathValue` to the user PATH, keeping the value type the registry already
# uses: [Environment]::SetEnvironmentVariable would rewrite it as REG_SZ and
# leave `%USERPROFILE%\...` entries unexpanded.
function Add-ToUserPath {
    param([Parameter(Mandatory)][string]$PathValue)

    $normalized = $PathValue.TrimEnd('\')
    $comparison = [System.StringComparison]::OrdinalIgnoreCase

    $key = Get-Item -Path $registryPath
    $kind = 'ExpandString'
    $current = $key.GetValue('Path', $null, 'DoNotExpandEnvironmentNames')
    if ($null -eq $current) {
        $current = ''
    } else {
        $kind = $key.GetValueKind('Path').ToString()
    }

    $entries = @($current -split ';' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    if (-not ($entries | Where-Object { $_.TrimEnd('\').Equals($normalized, $comparison) })) {
        $entries += $normalized
        Set-ItemProperty -Path $registryPath -Name Path -Value ($entries -join ';') -Type $kind
        Send-EnvironmentChange
    }

    # The terminal running the installer too, so `fzv` works in it at once.
    $session = @($env:Path -split ';' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    if (-not ($session | Where-Object { $_.TrimEnd('\').Equals($normalized, $comparison) })) {
        $env:Path = (($session + $normalized) -join ';')
    }
}

$target = Get-FzvArchitecture
$archiveName = "fzv-windows-$target.zip"
$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("fzv-install-" + [System.IO.Path]::GetRandomFileName())
$archivePath = Join-Path $temporary $archiveName
$sumsPath = Join-Path $temporary 'SHA256SUMS'

try {
    New-Item -ItemType Directory -Force -Path $temporary | Out-Null

    Write-Host "Downloading $archiveName..."
    Get-RemoteFile -Uri "$releaseBase/$archiveName" -OutFile $archivePath
    Get-RemoteFile -Uri "$releaseBase/SHA256SUMS" -OutFile $sumsPath

    $expected = Get-PublishedHash -SumsPath $sumsPath -Name $archiveName
    if ($expected -notmatch '^[0-9a-f]{64}$') {
        throw "SHA256SUMS does not carry a SHA-256 for $archiveName"
    }
    $actual = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "$archiveName does not match its published SHA-256 (expected $expected, got $actual)"
    }
    Write-Host "Verified SHA-256 $($expected.Substring(0, 12))..."

    $onPath = Get-Command -Name fzv.exe -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1

    # `zig.exe` and `zls.exe` next to fzv.exe are copies of it that follow the
    # active version, so replacing the directory has to put them back: an
    # upgrade must not be what breaks `zig` in every terminal.
    $shims = @('zig.exe', 'zls.exe') | Where-Object { Test-Path -Path (Join-Path $InstallDir $_) }

    if (Test-Path -Path $InstallDir) {
        try {
            Remove-Item -Path $InstallDir -Recurse -Force
        } catch {
            throw "cannot replace $InstallDir - is fzv running? ($($_.Exception.Message))"
        }
    }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Expand-Archive -Path $archivePath -DestinationPath $InstallDir -Force

    $executable = Join-Path $InstallDir 'fzv.exe'
    if (-not (Test-Path -Path $executable)) {
        throw "$archiveName does not contain fzv.exe"
    }
    foreach ($shim in $shims) {
        Copy-Item -Path $executable -Destination (Join-Path $InstallDir $shim) -Force
    }

    Add-ToUserPath -PathValue $InstallDir

    Write-Host "Installed fzv to $InstallDir"
    if ($onPath -and $onPath.Source -and $onPath.Source -ne $executable) {
        Write-Host "note: another fzv is already on PATH ($($onPath.Source)); remove it if this install should be the one that runs"
    }
    Write-Host "This terminal can use fzv already; other programs see it from their next start."
    Write-Host ""
    Write-Host "Install Zig with:"
    Write-Host "  fzv get dev, stable -path D:\zig"
} finally {
    if (Test-Path -Path $temporary) {
        Remove-Item -Path $temporary -Recurse -Force -ErrorAction SilentlyContinue
    }
}
