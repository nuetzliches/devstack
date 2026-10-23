<#
.SYNOPSIS
    Submit rendered winget manifests to microsoft/winget-pkgs, unless that
    would only add a duplicate.

.DESCRIPTION
    Uses the token in WINGET_CREATE_GITHUB_TOKEN (the secret WINGET_TOKEN,
    of the account whose fork of winget-pkgs carries the pull requests).

    It does not submit, and says why, when:
    - this version is in the winget catalog already;
    - an open pull request of that account names this version already;
    - the package is not in the catalog yet and a pull request of that
      account for it is open: until a moderator merges the first one, every
      release would add another "New package" pull request. Once the package
      is in, the winget workflow (or the next release) submits the version
      then current as an update. -Force submits anyway.

    Otherwise it syncs the fork (wingetcreate fails on a fork far behind;
    winget-pkgs moves thousands of commits a day), downloads wingetcreate
    pinned by version and checksum, and submits: "New package: ..." while
    the package is not in the catalog, "New version: ..." after.

    -DryRun decides and says what it would do, without syncing or
    submitting. -Owner names the account to look for (default: the token's).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Manifests,
    [Parameter(Mandatory)][string]$Version,
    [switch]$Force,
    [switch]$DryRun,
    [string]$Owner,
    [string]$WingetCreateVersion = '1.12.13.0',
    [string]$WingetCreateSha256 = '24042BD37915805615E6CF969AC57C6439124C3FE85823327F5F3FB24BD9FFEA'
)
$ErrorActionPreference = 'Stop'

$id = 'nuetzliches.devstack'
$catalog = 'repos/microsoft/winget-pkgs/contents/manifests/n/nuetzliches/devstack'

function Skip([string]$why) {
    "::warning::not submitted to winget: $why"
    $global:LASTEXITCODE = 0
    exit 0
}

# gh exits non-zero for a 404; that is an answer here, not a failure (and
# Windows PowerShell would stop on its stderr line otherwise).
function Exists([string]$path) {
    $ErrorActionPreference = 'Continue'
    gh api $path --silent 2>$null
    $found = $LASTEXITCODE -eq 0
    $global:LASTEXITCODE = 0
    return $found
}

if (-not $DryRun) {
    if (-not $env:WINGET_CREATE_GITHUB_TOKEN) { Skip 'no WINGET_TOKEN' }
    $env:GH_TOKEN = $env:WINGET_CREATE_GITHUB_TOKEN
}
if (-not $Owner) { $Owner = gh api user --jq .login }

$inCatalog = Exists $catalog
if ($inCatalog -and (Exists "$catalog/$Version")) { Skip "$id $Version is in the catalog already" }

$open = gh pr list -R microsoft/winget-pkgs --author $Owner --state open --search "$id in:title" --json number,title | ConvertFrom-Json
$global:LASTEXITCODE = 0
$same = @($open | Where-Object { $_.title -match "(^|\s)$([regex]::Escape($Version))$" })
if ($same.Count) { Skip "#$($same[0].number) submits $id $Version already" }
if (-not $inCatalog -and @($open).Count -and -not $Force) {
    $list = ($open | ForEach-Object { "#$($_.number)" }) -join ', '
    Skip "$id is not in the catalog yet and $list adds it; once merged, submit the version then current (winget workflow), or pass -Force"
}

$kind = if ($inCatalog) { 'New version' } else { 'New package' }
$title = "${kind}: $id version $Version"
if ($DryRun) {
    "would submit: $title (from $Owner's fork)"
    exit 0
}

$fork = "repos/$Owner/winget-pkgs"
if (Exists $fork) {
    $ErrorActionPreference = 'Continue'
    gh api -X POST "$fork/merge-upstream" -f branch=master --jq .message
    $ErrorActionPreference = 'Stop'
    if ($LASTEXITCODE -ne 0) { "::warning::could not sync $Owner/winget-pkgs with microsoft/winget-pkgs" }
    $global:LASTEXITCODE = 0
}

$exe = Join-Path ([IO.Path]::GetTempPath()) 'wingetcreate.exe'
Invoke-WebRequest "https://github.com/microsoft/winget-create/releases/download/v$WingetCreateVersion/wingetcreate.exe" -OutFile $exe -UseBasicParsing
$hash = (Get-FileHash $exe -Algorithm SHA256).Hash
if ($hash -ne $WingetCreateSha256) { throw "wingetcreate.exe: checksum $hash, expected $WingetCreateSha256" }
& $exe submit --no-open --prtitle $title $Manifests
if ($LASTEXITCODE -ne 0) { throw "wingetcreate failed ($LASTEXITCODE)" }
