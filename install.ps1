# Install a checksum-verified native Windows release without administrator access.
[CmdletBinding()]
param(
    [string]$Version = 'latest',
    [string]$Archive,
    [string]$Sha256,
    [string]$Prefix = "$env:LOCALAPPDATA\Preflight\bin",
    [string]$Config = "$env:LOCALAPPDATA\Preflight\preflight.toml",
    [string]$Data = "$env:LOCALAPPDATA\Preflight\data"
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT' -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'This installer requires Windows x86_64 and 64-bit PowerShell.'
}
if ([bool]$Archive -ne [bool]$Sha256) { throw 'Supply both Archive and Sha256, or neither.' }
if ($Version -ne 'latest' -and $Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
    throw 'Version must be latest or vMAJOR.MINOR.PATCH.'
}
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('preflight-' + [guid]::NewGuid())
[void][IO.Directory]::CreateDirectory($temporary)
try {
    if (!$Archive) {
        $base = 'https://github.com/MaxLaneDev/preflight/releases/'
        if ($Version -eq 'latest') { $base += 'latest/download' }
        else { $base += "download/$Version" }
        $checksums = Join-Path $temporary 'SHA256SUMS'
        & curl.exe --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 10 --max-time 180 --max-filesize 1048576 "$base/SHA256SUMS" -o $checksums
        if ($LASTEXITCODE -ne 0) { throw 'Cannot download release checksums.' }
        $seen = @{}; $selected = @()
        foreach ($line in [IO.File]::ReadAllLines($checksums)) {
            if ($line -cnotmatch '^([0-9a-f]{64})  ([A-Za-z0-9_.-]+)$') { throw 'Invalid release checksum.' }
            $digest = $Matches[1]; $name = $Matches[2]
            if ($seen.ContainsKey($name)) { throw 'Duplicate release filename.' }
            $seen[$name] = $true
            if ($name -cmatch '^preflight-[0-9]+\.[0-9]+\.[0-9]+-windows-x86_64\.tar\.gz$') {
                $selected += @{name=$name; sha=$digest}
            }
        }
        if ($selected.Count -ne 1) { throw 'Expected one Windows x86_64 release.' }
        $Archive = Join-Path $temporary $selected[0].name
        $Sha256 = $selected[0].sha
        & curl.exe --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 10 --max-time 180 --max-filesize 536870912 "$base/$($selected[0].name)" -o $Archive
        if ($LASTEXITCODE -ne 0) { throw 'Cannot download server archive.' }
    }
    if ($Sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Invalid SHA-256.' }
    if ((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant() -cne $Sha256) {
        throw 'Archive checksum mismatch.'
    }
    # The outer checksum is checked before executing the native payload verifier.
    & tar.exe -xzf $Archive -C $temporary preflight.exe
    if ($LASTEXITCODE -ne 0) { throw 'Release binary missing.' }
    $binary = Join-Path $temporary 'preflight.exe'
    & $binary package install $Archive --sha256 $Sha256 --prefix $Prefix --config $Config --data $Data
    if ($LASTEXITCODE -ne 0) { throw 'Native package verification or installation failed.' }
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force
}
