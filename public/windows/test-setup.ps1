param([Parameter(Mandatory=$true)][string]$Archive)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Join-Path ([IO.Path]::GetTempPath()) ('colposcopy-install-test-' + [guid]::NewGuid())
$oldData = $env:LOCALAPPDATA
try {
    New-Item -ItemType Directory -Path $root | Out-Null
    $env:LOCALAPPDATA = Join-Path $root 'Local'
    $unpacked = Join-Path $root 'package'
    Expand-Archive -LiteralPath $Archive -DestinationPath $unpacked
    $setup = Join-Path $unpacked 'Setup.ps1'
    & $setup

    $installed = Join-Path $env:LOCALAPPDATA 'Programs/Colposcopy/Colposcopy.exe'
    $expected = (Get-FileHash -LiteralPath (Join-Path $unpacked 'Colposcopy.exe')).Hash
    if ((Get-FileHash -LiteralPath $installed).Hash -ne $expected) { throw 'Installed executable differs from package' }

    $marker = Join-Path $env:LOCALAPPDATA 'Colposcopy/synthetic-local-data.txt'
    New-Item -ItemType Directory -Path (Split-Path -Parent $marker) -Force | Out-Null
    Set-Content -LiteralPath $marker -Value 'synthetic fixture'
    & $setup
    if ((Get-Content -LiteralPath $marker -Raw).Trim() -ne 'synthetic fixture') { throw 'Reinstallation changed local data' }

    Add-Content -LiteralPath (Join-Path $unpacked 'README.txt') -Value 'synthetic corruption'
    $rejected = $false
    try { & $setup } catch {
        if ($_.Exception.Message -notmatch 'Package integrity check failed') { throw }
        $rejected = $true
    }
    if (-not $rejected -or (Get-FileHash -LiteralPath $installed).Hash -ne $expected) {
        throw 'Damaged package was accepted or changed the previous installation'
    }

    $process = Start-Process -FilePath $installed -WorkingDirectory (Split-Path -Parent $installed) -PassThru
    try {
        $ready = $false
        for ($attempt = 0; $attempt -lt 30; $attempt++) {
            $process.Refresh()
            if ($process.HasExited) { throw "The installed application exited with code $($process.ExitCode)" }
            if ($process.MainWindowHandle -ne 0) { $ready = $true; break }
            Start-Sleep -Seconds 1
        }
        if (-not $ready) { throw 'The installed application did not open a window' }
    } finally {
        if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    }
    Write-Output 'PASS Windows install, reinstall, local data preservation, damaged package rejection and application window'
} finally {
    $env:LOCALAPPDATA = $oldData
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
