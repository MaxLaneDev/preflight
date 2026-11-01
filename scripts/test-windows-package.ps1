# Native installation and Web/MCP cold-start acceptance in an isolated directory.
param([Parameter(Mandatory=$true)][string]$Packages, [string]$Installer)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path -Parent $PSScriptRoot
if (!$Installer) { $Installer = Join-Path $root 'install.ps1' }
$packagesFound = @(Get-ChildItem -LiteralPath $Packages -Filter '*-windows-x86_64.tar.gz')
if ($packagesFound.Count -ne 1) { throw 'Expected one Windows package.' }
$archive = $packagesFound[0].FullName
$sha = ([IO.File]::ReadAllText("$archive.sha256") -split '\s+')[0]
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('preflight-test-' + [guid]::NewGuid())
[void][IO.Directory]::CreateDirectory($temporary)
$server = $null
try {
    $prefix = Join-Path $temporary 'binary with spaces'
    $config = Join-Path $temporary 'settings\preflight.toml'
    $data = Join-Path $temporary 'data'
    $build = if ($env:PREFLIGHT_BINARY) { $env:PREFLIGHT_BINARY } else { Join-Path $root 'target\release\preflight.exe' }
    $failed = $false
    try { & $Installer -Archive $archive -Sha256 ('0' * 64) -Prefix $prefix -Config $config -Data $data }
    catch { $failed = $true }
    if (!$failed -or (Test-Path -LiteralPath $prefix)) { throw 'Invalid checksum changed installation.' }
    $receipt = (& $Installer -Archive $archive -Sha256 $sha -Prefix $prefix -Config $config -Data $data) | ConvertFrom-Json
    $binary = $receipt.binary
    $version = (& $binary --version) | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $version.os -ne 'windows' -or $version.arch -ne 'x86_64') { throw 'Wrong installed platform.' }
    $original = [IO.File]::ReadAllText($config)
    & $Installer -Archive $archive -Sha256 $sha -Prefix $prefix -Config $config -Data $data | Out-Null
    if ([IO.File]::ReadAllText($config) -cne $original) { throw 'Reinstallation changed configuration.' }
    $port = (& $build package free-port).Trim()
    [IO.File]::WriteAllText($config, $original.Replace('127.0.0.1:4170', "127.0.0.1:$port"))
    $server = Start-Process -FilePath $binary -ArgumentList @('--config', ('"' + $config + '"'), 'serve') -PassThru -RedirectStandardOutput "$temporary\server.log" -RedirectStandardError "$temporary\server.err"
    $base = "http://127.0.0.1:$port"
    $ready = $false
    for ($attempt = 0; $attempt -lt 200; $attempt++) {
        if ($server.HasExited) { throw 'Installed server exited.' }
        try { Invoke-RestMethod "$base/health" -TimeoutSec 1 | Out-Null; $ready = $true; break } catch { Start-Sleep -Milliseconds 100 }
    }
    if (!$ready) { throw 'Installed server readiness timed out.' }
    foreach ($path in @('/', '/app.js', '/app.css', '/executions')) {
        $response = Invoke-WebRequest "$base$path" -TimeoutSec 10
        if ($response.StatusCode -ne 200) { throw "Bundled UI missing: $path" }
    }
    $capabilities = Invoke-RestMethod "$base/api/v1/capabilities" -TimeoutSec 10
    if (!$capabilities.features.bundled_web_ui) { throw 'Bundled UI capability missing.' }
    $headers = @{Accept='application/json, text/event-stream'}
    $body = @{jsonrpc='2.0';id=1;method='initialize';params=@{protocolVersion='2025-03-26';capabilities=@{};clientInfo=@{name='windows-package-check';version='1'}}} | ConvertTo-Json -Depth 8 -Compress
    $mcp = Invoke-WebRequest "$base/mcp" -Method Post -Headers $headers -ContentType application/json -Body $body -TimeoutSec 10
    if ($mcp.StatusCode -ne 200 -or $mcp.Content -notmatch 'protocolVersion') { throw 'Installed MCP initialization failed.' }
    & $build package uninstall --prefix $prefix 2> "$temporary\busy.err" | Out-Null
    if ($LASTEXITCODE -eq 0 -or !(Test-Path -LiteralPath $prefix)) { throw 'Uninstall did not protect the running server.' }
    Stop-Process -Id $server.Id; $server.WaitForExit(); $server = $null
    & $build package uninstall --prefix $prefix | Out-Null
    if ($LASTEXITCODE -ne 0 -or (Test-Path -LiteralPath $prefix)) { throw 'Uninstall failed.' }
    if (!(Test-Path -LiteralPath $config) -or !(Test-Path -LiteralPath "$data\preflight.sqlite3")) { throw 'Uninstall removed user data.' }
    Write-Output 'Windows native archive install/reinstall, UI, API, MCP and data-preserving uninstall passed.'
} catch {
    Get-ChildItem -LiteralPath $temporary -Filter 'server.*' | ForEach-Object { Get-Content -LiteralPath $_.FullName -Tail 40 }
    throw
} finally {
    if ($server -and !$server.HasExited) { Stop-Process -Id $server.Id; $server.WaitForExit() }
    Remove-Item -LiteralPath $temporary -Recurse -Force
}
