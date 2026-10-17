<#
.SYNOPSIS
    Install or update devstack on Windows.

.DESCRIPTION
    Downloads the Windows build of a devstack release (with a progress bar),
    checks it against the release's SHA256SUMS and puts devstack.exe into a
    directory on the user's PATH (default %LOCALAPPDATA%\Programs\devstack).

    Downloads anonymously from GitHub; falls back to `gh` when that fails and
    `gh` is installed and logged in (a private fork).

    What the checksum proves: the archive is the one listed in the release's
    SHA256SUMS, i.e. it was not damaged or swapped on the way. It does not
    prove who built it, since both files come from the same release. When
    `gh` is installed and logged in, the installer also checks the archive's
    build provenance (`gh attestation verify`): that it was built by this
    repository's release workflow. Releases before 0.9.9 have none.

    The hub integration (start at logon, devstack:// link) is skipped with
    -NoHubIntegration or DEVSTACK_NO_HUB_INTEGRATION=1 (the variable works
    with `irm | iex`, which takes no parameters); the choice is kept, so
    `devstack up` does not register them later either.

    A first install opens the dashboard at the end. An update moves a running
    dashboard hub onto the new binary; running stacks keep their daemon until
    they restart (`devstack down`, `devstack up`, or "Restart stack"). The
    running devstack.exe is renamed out of the way (Windows allows that, not
    overwriting) and removed on the next install.

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
    [switch]$NoHubIntegration,
    # Do not open the dashboard after a first install.
    [switch]$NoOpen
)
$ErrorActionPreference = 'Stop'
# `irm | iex` passes no parameters: the opt-out also comes as a variable.
if ($env:DEVSTACK_NO_HUB_INTEGRATION -and $env:DEVSTACK_NO_HUB_INTEGRATION -ne '0') { $NoHubIntegration = $true }
# The first release whose archives carry build provenance (attestations).
$firstAttested = [version]'0.9.9'
# Windows PowerShell 5.1 started from PowerShell 7 inherits 7's PSModulePath,
# tries 7's modules and cannot load them: Get-FileHash and friends go missing
# (#12). Keep 5.1 to its own module directories; below, .NET does what
# module cmdlets would (hash, unzip, HTTP), so the script runs the same in both.
if ($PSVersionTable.PSEdition -eq 'Desktop') {
    $env:PSModulePath = (($env:PSModulePath -split ';') | Where-Object { $_ -and $_ -notmatch '\\PowerShell\\' }) -join ';'
}
# Invoke-WebRequest's own progress bar slows Windows PowerShell 5.1 down to a
# crawl; the download below draws its own.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$asset = 'devstack-x86_64-pc-windows-msvc.zip'
$tag = if ($Version) { 'v' + $Version.TrimStart('v') } else { $null }
$steps = 4
$script:step = 0

function Step([string]$text) {
    $script:step++
    Write-Host ''
    Write-Host ("  [{0}/{1}] " -f $script:step, $steps) -ForegroundColor Cyan -NoNewline
    Write-Host $text
}

function Note([string]$text) {
    Write-Host "        $text" -ForegroundColor DarkGray
}

# Download with a progress line: bytes, percent, speed.
function Get-File([string]$Uri, [string]$OutFile, [switch]$Quiet) {
    $req = [Net.HttpWebRequest]::Create($Uri)
    $req.UserAgent = 'devstack-installer'
    $resp = $req.GetResponse()
    try {
        $total = $resp.ContentLength
        $in = $resp.GetResponseStream()
        $out = [IO.File]::Create($OutFile)
        try {
            $buf = New-Object byte[] 262144
            $done = [long]0
            $clock = [Diagnostics.Stopwatch]::StartNew()
            $drawn = [long]-1000
            $width = 28
            $full = [string][char]0x2588
            $empty = [string][char]0x2591
            $draw = {
                if ($Quiet) { return }
                $mb = $done / 1MB
                $speed = if ($clock.Elapsed.TotalSeconds -gt 0) { $mb / $clock.Elapsed.TotalSeconds } else { 0 }
                if ($total -gt 0) {
                    $frac = [Math]::Min([double]1, [double]$done / [double]$total)
                    $n = [int][Math]::Round($frac * $width)
                    $line = "        {0}{1} {2,3}%  {3:N1} / {4:N1} MB  {5:N1} MB/s" -f ($full * $n), ($empty * ($width - $n)), [int]($frac * 100), $mb, ($total / 1MB), $speed
                }
                else {
                    $line = "        {0:N1} MB  {1:N1} MB/s" -f $mb, $speed
                }
                [Console]::Write("`r$line   ")
            }
            while (($n = $in.Read($buf, 0, $buf.Length)) -gt 0) {
                $out.Write($buf, 0, $n)
                $done += $n
                if ($clock.ElapsedMilliseconds - $drawn -ge 100) {
                    & $draw
                    $drawn = $clock.ElapsedMilliseconds
                }
            }
            & $draw
            if (-not $Quiet) { [Console]::WriteLine() }
        }
        finally {
            $out.Dispose()
            $in.Dispose()
        }
        return $resp.ResponseUri.AbsoluteUri
    }
    finally {
        $resp.Dispose()
    }
}

# The tag `latest` points at: github.com answers /releases/latest with a
# redirect to /releases/tag/<tag> (no API call, no rate limit).
function Get-LatestTag {
    try {
        $req = [Net.HttpWebRequest]::Create("https://github.com/$Repo/releases/latest")
        $req.UserAgent = 'devstack-installer'
        $req.AllowAutoRedirect = $false
        $resp = $req.GetResponse()
        try {
            if ($resp.Headers['Location'] -match '/tag/(v[^/?#]+)') { return $Matches[1] }
        }
        finally { $resp.Dispose() }
    }
    catch {}
    return $null
}

function Test-Hub {
    # The hub's port, as devstack itself takes it.
    $port = if ($env:DEVSTACK_HUB_PORT) { $env:DEVSTACK_HUB_PORT } else { '4250' }
    try {
        $req = [Net.HttpWebRequest]::Create("http://127.0.0.1:$port/hub/ping")
        $req.Timeout = 2000
        $req.GetResponse().Dispose()
        return $true
    }
    catch { return $false }
}

# Build provenance (GitHub artifact attestation) of the archive, when `gh` is
# installed and logged in: it proves the archive was built by this
# repository's release workflow, which the checksum alone cannot.
function Test-Provenance([string]$File) {
    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if (-not $gh) { Note 'provenance not checked (no gh; https://cli.github.com)'; return }
    & gh auth status *> $null
    if ($LASTEXITCODE -ne 0) { Note 'provenance not checked (gh is not logged in)'; return }
    if ($tag) {
        $v = $null
        if ([version]::TryParse(($tag.TrimStart('v') -split '-')[0], [ref]$v) -and $v -lt $firstAttested) {
            Note "provenance not checked ($tag predates attestations)"
            return
        }
    }
    & gh attestation verify $File --repo $Repo *> $null
    if ($LASTEXITCODE -ne 0) {
        throw "the build provenance of $asset does not verify (gh attestation verify $asset --repo $Repo)"
    }
    Note "built by $Repo's release workflow (attestation verified)"
}

# The user's PATH as stored: unexpanded, with its registry type kept
# (REG_EXPAND_SZ stays REG_EXPAND_SZ); [Environment]::SetEnvironmentVariable
# would flatten `%USERPROFILE%\...` entries.
function Add-UserPath([string]$Entry) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try {
        $path = [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
        $norm = $Entry.TrimEnd('\')
        $present = $path -split ';' | Where-Object { $_ -and ([Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -eq $norm) }
        if ($present) { return $false }
        $kind = if ($path) { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
        $key.SetValue('Path', (($path.TrimEnd(';') + ";$Entry").TrimStart(';')), $kind)
    }
    finally { $key.Close() }
    # Tell running programs (Explorer, new terminals) that the environment changed.
    [Environment]::SetEnvironmentVariable('DEVSTACK_PATH_BROADCAST', '1', 'User')
    [Environment]::SetEnvironmentVariable('DEVSTACK_PATH_BROADCAST', $null, 'User')
    return $true
}

# `devstack --version` of a binary a virus scanner may still hold: a few tries.
function Get-InstalledVersion([string]$Exe) {
    for ($i = 1; ; $i++) {
        try {
            $v = & $Exe --version
            if ($LASTEXITCODE -eq 0 -and $v) { return $v }
            throw "exit $LASTEXITCODE"
        }
        catch {
            if ($i -ge 5) { throw "$Exe --version failed: $($_.Exception.Message)" }
            Start-Sleep -Milliseconds (400 * $i)
        }
    }
}

Write-Host ''
Write-Host '  devstack' -ForegroundColor White -NoNewline
Write-Host ' installer' -ForegroundColor DarkGray

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("devstack-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $target = Join-Path $Dir 'devstack.exe'
    $firstInstall = -not (Test-Path $target)
    $hubWasRunning = Test-Hub

    # 1. Download
    if (-not $tag) { $tag = Get-LatestTag }
    $base = if ($tag) { "https://github.com/$Repo/releases/download/$tag" } else { "https://github.com/$Repo/releases/latest/download" }
    Step ("Downloading devstack " + $(if ($tag) { $tag } else { '(latest release)' }))
    try {
        $null = Get-File -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
        $null = Get-File -Uri "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS') -Quiet
    }
    catch {
        $gh = Get-Command gh -ErrorAction SilentlyContinue
        if (-not $gh) { throw }
        Note "direct download failed ($($_.Exception.Message)); trying gh"
        $ghArgs = @('release', 'download')
        if ($tag) { $ghArgs += $tag }
        $ghArgs += @('-R', $Repo, '-p', $asset, '-p', 'SHA256SUMS', '-D', $tmp, '--clobber')
        & gh @ghArgs
        if ($LASTEXITCODE -ne 0) { throw "gh release download failed (exit $LASTEXITCODE)" }
    }

    # 2. Verify
    Step 'Checking the download against the release checksums'
    $expected = (Get-Content (Join-Path $tmp 'SHA256SUMS') |
        Where-Object { $_ -match "\s\*?$([regex]::Escape($asset))$" } |
        ForEach-Object { ($_ -split '\s+')[0] }) | Select-Object -First 1
    $sha = [Security.Cryptography.SHA256]::Create()
    $fs = [IO.File]::OpenRead((Join-Path $tmp $asset))
    try { $actual = -join ($sha.ComputeHash($fs) | ForEach-Object { $_.ToString('X2') }) }
    finally { $fs.Dispose(); $sha.Dispose() }
    if (-not $expected -or $actual -ne $expected.ToUpperInvariant()) {
        throw "checksum mismatch for $asset (expected $expected, got $actual)"
    }
    Note 'SHA-256 matches (the archive is the one the release lists)'
    Test-Provenance (Join-Path $tmp $asset)

    # 3. Install
    Step "Installing to $Dir"
    $null = [Reflection.Assembly]::Load('System.IO.Compression.FileSystem, Version=4.0.0.0, Culture=neutral, PublicKeyToken=b77a5c561934e089')
    $unpacked = Join-Path $tmp 'unpacked'
    [IO.Compression.ZipFile]::ExtractToDirectory((Join-Path $tmp $asset), $unpacked)
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    Get-ChildItem $Dir -Filter 'devstack.exe.old*' -ErrorAction SilentlyContinue |
        ForEach-Object { Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue }
    # Next to the target first, complete, then two renames: the target is
    # never a half-written file, and a failed rename puts the old one back.
    # A running daemon holds the old file: renaming works, overwriting does not.
    $new = "$target.new"
    Copy-Item (Join-Path $unpacked 'devstack.exe') $new -Force
    $old = "$target.old-$([DateTime]::Now.ToString('yyyyMMddHHmmss'))"
    $moved = Test-Path $target
    if ($moved) { Move-Item $target $old -Force }
    try { Move-Item $new $target -Force }
    catch {
        if ($moved) { Move-Item $old $target -Force -ErrorAction SilentlyContinue }
        Remove-Item $new -Force -ErrorAction SilentlyContinue
        throw
    }
    $installed = Get-InstalledVersion $target
    Note $installed

    if (Add-UserPath $Dir) { Note 'added to your PATH' }
    # This terminal too (`irm | iex` runs in it), not only new ones.
    if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -eq $Dir.TrimEnd('\') })) { $env:Path = $env:Path.TrimEnd(';') + ";$Dir" }

    # 4. Dashboard
    Step 'Starting the dashboard'
    if (-not $NoHubIntegration) {
        # The dashboard hub starts at logon, and the dashboard can start it
        # (devstack:// link). Both in the user's registry hive, no admin rights.
        & $target hub install | Out-Null
        if ($LASTEXITCODE -ne 0) { Write-Warning "could not register the hub with your session (devstack hub install)" }
    }
    else {
        # The opt-out lasts: `hub uninstall` removes a registration an earlier
        # install made and leaves the marker `devstack up` respects.
        & $target hub uninstall | Out-Null
        if ($LASTEXITCODE -eq 0) { Note 'no start at logon, no devstack:// link (devstack hub install adds them)' }
        else { Write-Warning "could not record the opt-out (devstack hub uninstall)" }
    }
    if ($hubWasRunning) {
        # The hub runs the old binary: move it onto the new one.
        & $target hub stop | Out-Null
        for ($i = 0; $i -lt 25 -and (Test-Hub); $i++) { Start-Sleep -Milliseconds 200 }
    }
    if ($firstInstall -and -not $NoOpen) {
        & $target ui | Out-Null
        if ($LASTEXITCODE -eq 0) { Note 'opened in your browser' }
        else { Note 'run `devstack ui` to open it' }
    }
    else {
        & $target hub start | Out-Null
        Note 'running; `devstack ui` opens it'
    }

    Write-Host ''
    Write-Host '  Done. ' -ForegroundColor Green -NoNewline
    Write-Host "$installed is installed."
    if ($firstInstall) {
        Write-Host '  Next, in your repository:' -ForegroundColor DarkGray
        Write-Host '    devstack doctor    ' -NoNewline; Write-Host 'what this machine still needs' -ForegroundColor DarkGray
        Write-Host '    devstack up        ' -NoNewline; Write-Host 'start the stack' -ForegroundColor DarkGray
    }
    else {
        Write-Host '  Running stacks keep their version until they restart ("Restart stack", or devstack down + up).' -ForegroundColor DarkGray
    }
    Write-Host ''
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
