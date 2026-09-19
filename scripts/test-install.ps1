#requires -version 5.1
<#
.SYNOPSIS
    Installs fzv from a packaged release, the way a user does.

.DESCRIPTION
    Packages a built fzv.exe with scripts/package-windows.ps1 - the packager the
    release workflow uses - and runs install.ps1 against the result, so the asset
    names, the checksum file and the PATH handling are exercised together, and a
    tampered archive is shown to be refused.

    fzv is installed into a scratch directory and the user PATH is put back
    afterwards, so the machine is left as it was.

    From the repository root, after `cargo build --release`:

        pwsh -File scripts\test-install.ps1

.PARAMETER Binary
    The fzv.exe to package. Defaults to target\release\fzv.exe.

.PARAMETER BaseUrl
    Install from an existing release instead of the local package, for example
    https://github.com/ref42/fzv/releases/latest/download - which is also the way
    to check a published release.
#>
[CmdletBinding()]
param(
    [string]$Binary = '',
    [string]$BaseUrl = ''
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repository = Split-Path -Parent $PSScriptRoot
if (-not $Binary) {
    $Binary = Join-Path $repository 'target\release\fzv.exe'
}
$expectedVersion = (Select-String -Path (Join-Path $repository 'Cargo.toml') -Pattern '^version = "(.+)"$').Matches[0].Groups[1].Value
$installer = Join-Path $repository 'install.ps1'

$work = Join-Path ([System.IO.Path]::GetTempPath()) ("fzv-install-test-" + [System.IO.Path]::GetRandomFileName())
$dist = Join-Path $work 'release'
$install = Join-Path $work 'installed'
$stable = Join-Path $dist 'fzv-windows-x86_64.zip'
New-Item -ItemType Directory -Force -Path $work, $dist | Out-Null

$registryPath = 'HKCU:\Environment'
$savedKind = 'ExpandString'
$savedPath = (Get-Item -Path $registryPath).GetValue('Path', $null, 'DoNotExpandEnvironmentNames')
if ($null -ne $savedPath) {
    $savedKind = (Get-Item -Path $registryPath).GetValueKind('Path').ToString()
}
$savedSessionPath = $env:Path

try {
    if ($BaseUrl) {
        $url = $BaseUrl.TrimEnd('/')
    } else {
        if (-not (Test-Path -Path $Binary -PathType Leaf)) {
            throw "nothing to install: $Binary does not exist (build it with 'cargo build --release')"
        }
        & (Join-Path $PSScriptRoot 'package-windows.ps1') -Tag 'v0.0.0-test' -Arch 'x86_64' -Binary $Binary -OutputDir $dist | Out-Null
        # The release writes SHA256SUMS over its assets on Linux; this is that
        # same `<hash>  <name>` format.
        $hash = (Get-FileHash -Algorithm SHA256 -Path $stable).Hash.ToLowerInvariant()
        "$hash  fzv-windows-x86_64.zip" | Out-File -FilePath (Join-Path $dist 'SHA256SUMS') -Encoding ascii
        $url = 'file:///' + ($dist -replace '\\', '/')
    }
    Write-Host "installing from $url"

    # A `%USERPROFILE%` entry in the PATH proves the installer keeps the registry
    # value type: [Environment]::SetEnvironmentVariable would rewrite the value as
    # REG_SZ and leave this one unexpanded.
    $probe = '%USERPROFILE%\fzv-path-probe'
    $entries = @()
    if ($savedPath) {
        $entries = @($savedPath -split ';' | ForEach-Object { $_.Trim() } | Where-Object { $_ -and $_ -ne $probe })
    }
    Set-ItemProperty -Path $registryPath -Name Path -Value ((@($probe) + $entries) -join ';') -Type ExpandString

    & $installer -BaseUrl $url -InstallDir $install

    $installed = Join-Path $install 'fzv.exe'
    if (-not (Test-Path -Path $installed -PathType Leaf)) {
        throw "fzv.exe was not installed into $install"
    }
    $version = (& $installed v).Trim()
    if ($version -ne $expectedVersion) {
        throw "the installed fzv reports '$version', expected '$expectedVersion'"
    }

    $after = (Get-Item -Path $registryPath).GetValue('Path', $null, 'DoNotExpandEnvironmentNames')
    $afterKind = (Get-Item -Path $registryPath).GetValueKind('Path').ToString()
    $afterEntries = @($after -split ';' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    if (-not ($afterEntries | Where-Object { $_.TrimEnd('\') -ieq $install.TrimEnd('\') })) {
        throw "the install directory was not added to the user PATH: $after"
    }
    if ($afterKind -ne 'ExpandString') {
        throw "the PATH value type became $afterKind; %VAR% entries would stop being expanded"
    }
    if (-not ($afterEntries | Where-Object { $_ -ceq $probe })) {
        throw "the `%USERPROFILE%` entry was expanded or dropped: $after"
    }
    Write-Host "installed fzv $version into $install, PATH entry added, value type still $afterKind"

    # Re-running the installer is the other way to upgrade. The `zig.exe` next to
    # fzv.exe is a copy of it, so it has to come back as the new one: a stale or
    # missing shim would break `zig` in every terminal.
    $shim = Join-Path $install 'zig.exe'
    Set-Content -Path $shim -Value 'stale' -Encoding ascii
    & $installer -BaseUrl $url -InstallDir $install
    if ((Get-FileHash -Algorithm SHA256 -Path $shim).Hash -ne (Get-FileHash -Algorithm SHA256 -Path $installed).Hash) {
        throw 'the shim beside fzv.exe was not refreshed'
    }
    Write-Host 'a second install replaces fzv.exe and refreshes the shim beside it'

    if (-not $BaseUrl) {
        # A tampered archive has to be refused before anything is touched.
        Add-Content -Path $stable -Value 'tampered'
        $refused = $false
        try {
            & $installer -BaseUrl $url -InstallDir $install
        } catch {
            $refused = $true
            if ($_.Exception.Message -notmatch 'SHA-256') {
                throw "the tampered archive failed for the wrong reason: $($_.Exception.Message)"
            }
        }
        if (-not $refused) {
            throw 'a tampered archive was installed'
        }
        if ((& $installed v).Trim() -ne $expectedVersion) {
            throw 'the refused install replaced the working one'
        }
        Write-Host 'a tampered archive is refused, and the installed fzv is untouched'
    }

    Write-Host 'PASS'
} finally {
    if ($null -eq $savedPath) {
        Remove-ItemProperty -Path $registryPath -Name Path -ErrorAction SilentlyContinue
    } else {
        Set-ItemProperty -Path $registryPath -Name Path -Value $savedPath -Type $savedKind
    }
    $env:Path = $savedSessionPath
    Remove-Item -Path $work -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'the user PATH was restored'
}
