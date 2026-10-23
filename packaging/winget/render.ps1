<#
.SYNOPSIS
    Fill in the winget manifest templates of this directory for one release.

.DESCRIPTION
    Writes the three manifests of packaging/winget/ with the version, the
    release date and the SHA-256 of the Windows archive to -Out, checks that
    the archive holds devstack.exe at its root (what the installer manifest
    names), and with -Validate runs `winget validate` on them when winget is
    there. An older winget warns about a newer schema in the header comment
    and exits non-zero for that too: only a failed validation fails.

    Used by the release workflow (the archive it built) and by the winget
    workflow (the archive of a published release).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$Zip,
    [Parameter(Mandatory)][string]$Out,
    # yyyy-MM-dd; default: today (UTC).
    [string]$Date = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd'),
    [switch]$Validate
)
$ErrorActionPreference = 'Stop'

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "-Version ${Version}: expected x.y.z" }
$sha = (Get-FileHash $Zip -Algorithm SHA256).Hash
New-Item -ItemType Directory -Force $Out | Out-Null
foreach ($f in Get-ChildItem $PSScriptRoot -Filter *.yaml) {
    $text = [IO.File]::ReadAllText($f.FullName) -replace '\{\{version\}\}', $Version -replace '\{\{sha256\}\}', $sha -replace '\{\{date\}\}', $Date
    if ($text -match '\{\{') { throw "$($f.Name): a placeholder is left" }
    [IO.File]::WriteAllText((Join-Path $Out $f.Name), $text, (New-Object Text.UTF8Encoding $false))
}

# The archive holds what the manifest names.
$unpacked = Join-Path ([IO.Path]::GetTempPath()) ("winget-unpack-" + [guid]::NewGuid())
try {
    Expand-Archive $Zip -DestinationPath $unpacked
    if (-not (Test-Path (Join-Path $unpacked 'devstack.exe'))) { throw 'devstack.exe is not at the root of the archive' }
} finally {
    Remove-Item -Recurse -Force $unpacked -ErrorAction SilentlyContinue
}
Get-Content (Join-Path $Out 'nuetzliches.devstack.installer.yaml')

if ($Validate) {
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        "winget $(winget --version)"
        $result = winget validate --manifest $Out --disable-interactivity 2>&1 | Out-String
        $result
        if ($LASTEXITCODE -ne 0 -and $result -notmatch 'succeeded with warnings') { throw 'winget validate failed' }
    } else {
        'no winget here; wingetcreate checks the manifests when it submits them'
    }
}
$global:LASTEXITCODE = 0
