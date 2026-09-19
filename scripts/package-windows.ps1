#requires -version 5.1
<#
.SYNOPSIS
    Packages the Windows assets of one fzv release.

.DESCRIPTION
    Writes the three assets a release publishes for one architecture and returns
    their names, so the caller uploads exactly what was written:

        fzv-<tag>-windows-<arch>.exe   the bare executable, for a hand install
        fzv-<tag>-windows-<arch>.zip   fzv.exe at the root, what `fzv update` installs
        fzv-windows-<arch>.zip         the stable name install.ps1 downloads

    The stable name carries no tag on purpose: `install.ps1` is fetched from
    `releases/latest/download`, and the archive it needs next to it has to have a
    name that does not change from release to release.

.PARAMETER Tag
    The release tag, for example v0.1.0. A stand-in tag is fine when the only
    purpose is to test the installer.

.PARAMETER Arch
    x86_64 or aarch64.

.PARAMETER Binary
    The built fzv.exe to package.

.PARAMETER OutputDir
    Where the assets are written.

.EXAMPLE
    ./scripts/package-windows.ps1 -Tag v0.1.0 -Arch x86_64 `
        -Binary target/x86_64-pc-windows-msvc/release/fzv.exe -OutputDir .
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Tag,
    [Parameter(Mandatory)][ValidateSet('x86_64', 'aarch64')][string]$Arch,
    [Parameter(Mandatory)][string]$Binary,
    [Parameter(Mandatory)][string]$OutputDir
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -Path $Binary -PathType Leaf)) {
    throw "nothing to package: $Binary does not exist"
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$executable = Join-Path $OutputDir "fzv-$Tag-windows-$Arch.exe"
$archive = Join-Path $OutputDir "fzv-$Tag-windows-$Arch.zip"
$stable = Join-Path $OutputDir "fzv-windows-$Arch.zip"

Copy-Item -Path $Binary -Destination $executable -Force

# fzv.exe at the root of the archive: that is the layout `fzv update` expects.
$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("fzv-package-" + [System.IO.Path]::GetRandomFileName())
try {
    New-Item -ItemType Directory -Force -Path $staging | Out-Null
    Copy-Item -Path $Binary -Destination (Join-Path $staging 'fzv.exe') -Force
    Compress-Archive -Path (Join-Path $staging 'fzv.exe') -DestinationPath $archive -Force
} finally {
    Remove-Item -Path $staging -Recurse -Force -ErrorAction SilentlyContinue
}
Copy-Item -Path $archive -Destination $stable -Force

Write-Host "packaged $(Split-Path -Leaf $archive) and $(Split-Path -Leaf $stable)"

# The names, so the caller uploads what was actually written.
Split-Path -Leaf $executable
Split-Path -Leaf $archive
Split-Path -Leaf $stable
