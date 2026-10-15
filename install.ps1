<#
.SYNOPSIS
    Install or update devstack on Windows.

.DESCRIPTION
    Downloads the Windows build of a devstack release, checks it against the
    release's SHA256SUMS and puts devstack.exe into a directory on the user's
    PATH (default %LOCALAPPDATA%\Programs\devstack).

    Uses `gh` when it is installed and logged in (also for a private
    repository), otherwise downloads anonymously from GitHub.

    Updating while a daemon runs is fine: the running devstack.exe is renamed
    out of the way (Windows allows that, not overwriting) and removed on the
    next install. Restart the daemon (`devstack down`, `devstack up`) to use
    the new version.

.EXAMPLE
    irm https://github.com/nuetzliches/devstack/releases/latest/download/install.ps1 | iex

.EXAMPLE
    gh release download -R nuetzliches/devstack -p install.ps1 -D $env:TEMP --clobber
    & "$env:TEMP\install.ps1" -Version 0.1.0
#>
[CmdletBinding()]
param(
    # Release to install, e.g. 0.1.0; default: the latest.
    [string]$Version,
    [string]$Dir = (Join-Path $env:LOCALAPPDATA 'Programs\devstack'),
    [string]$Repo = 'nuetzliches/devstack',
    # Do not start the hub at logon and do not register the devstack:// link.
    [switch]$NoHubIntegration
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$asset = 'devstack-x86_64-pc-windows-msvc.zip'
$tag = if ($Version) { 'v' + $Version.TrimStart('v') } else { $null }
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("devstack-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $viaGh = $false
    if (Get-Command gh -ErrorAction SilentlyContinue) {
        gh auth status *> $null
        $viaGh = ($LASTEXITCODE -eq 0)
    }
    if ($viaGh) {
        $ghArgs = @('release', 'download')
        if ($tag) { $ghArgs += $tag }
        $ghArgs += @('-R', $Repo, '-p', $asset, '-p', 'SHA256SUMS', '-D', $tmp)
        & gh @ghArgs
        if ($LASTEXITCODE -ne 0) { throw "gh release download failed (exit $LASTEXITCODE)" }
    }
    else {
        $base = if ($tag) { "https://github.com/$Repo/releases/download/$tag" } else { "https://github.com/$Repo/releases/latest/download" }
        foreach ($f in @($asset, 'SHA256SUMS')) {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/$f" -OutFile (Join-Path $tmp $f)
        }
    }

    $expected = (Get-Content (Join-Path $tmp 'SHA256SUMS') |
        Where-Object { $_ -match "\s\*?$([regex]::Escape($asset))$" } |
        ForEach-Object { ($_ -split '\s+')[0] }) | Select-Object -First 1
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash
    if (-not $expected -or $actual -ne $expected.ToUpperInvariant()) {
        throw "checksum mismatch for $asset (expected $expected, got $actual)"
    }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    $target = Join-Path $Dir 'devstack.exe'
    Get-ChildItem $Dir -Filter 'devstack.exe.old*' -ErrorAction SilentlyContinue |
        ForEach-Object { Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue }
    if (Test-Path $target) {
        # A running daemon holds the file: renaming works, overwriting does not.
        Move-Item $target "$target.old-$([DateTime]::Now.ToString('yyyyMMddHHmmss'))" -Force
    }
    Copy-Item (Join-Path $tmp 'devstack.exe') $target

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($userPath -split ';') -contains $Dir)) {
        [Environment]::SetEnvironmentVariable('Path', (($userPath.TrimEnd(';') + ";$Dir").TrimStart(';')), 'User')
        Write-Host "Added $Dir to your PATH (new terminals see it)."
    }
    Write-Host "Installed: $(& $target --version) -> $target"
    if (-not $NoHubIntegration) {
        # The dashboard hub starts at logon, and the dashboard can start it
        # (devstack:// link). Both in the user's registry hive, no admin rights.
        & $target hub install
        if ($LASTEXITCODE -ne 0) { Write-Warning "could not register the hub with your session (devstack hub install)" }
    }
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
