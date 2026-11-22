param([string]$PrintSourcesDirectory = '')
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
if (-not $env:LOCALAPPDATA) {throw 'LOCALAPPDATA is required'}
$source=[IO.Path]::GetFullPath($PSScriptRoot)
$destination=Join-Path $env:LOCALAPPDATA 'Programs/Colposcopy'
$parent=Split-Path -Parent $destination
$manifest=Get-Content -LiteralPath (Join-Path $source 'package-manifest.json') -Raw | ConvertFrom-Json
if ($manifest.format -ne 1 -or @($manifest.files).Count -lt 4) {throw 'Invalid package manifest'}
$seen=New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
foreach ($file in $manifest.files) {
 if ($file.path -notmatch '^[A-Za-z0-9._+/-]+$' -or $file.path -match '(^/|(^|/)\.\.?(/|$)|//|/$)' -or -not $seen.Add([string]$file.path) -or $file.sha256 -cnotmatch '^[0-9a-f]{64}$') {throw 'Invalid package path or hash'}
 $path=Join-Path $source $file.path
 if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or ((Get-Item -LiteralPath $path).Attributes -band [IO.FileAttributes]::ReparsePoint)) {throw 'Missing file or unsupported link'}
 if ((Get-Item -LiteralPath $path).Length -ne $file.size -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -cne $file.sha256) {throw 'Package integrity check failed'}
}
foreach ($required in @('Colposcopy.exe','Install-PrintRuntime.ps1','print-runtime-manifest.json','licenses/LICENSE','licenses/FONT-LICENSE.txt','Setup.ps1')) {if (-not $seen.Contains($required)) {throw "Missing required file: $required"}}
if (Test-Path -LiteralPath $destination) {
 if ((Get-Item -LiteralPath $destination).Attributes -band [IO.FileAttributes]::ReparsePoint) {throw 'Installation directory must not be a link'}
 foreach ($process in @(Get-Process -Name Colposcopy -ErrorAction SilentlyContinue)) {throw 'Close Colposcopy before upgrading'}
}
& (Join-Path $source 'Install-PrintRuntime.ps1') -SourcesDirectory $PrintSourcesDirectory
New-Item -ItemType Directory -Force $parent | Out-Null
$stage=Join-Path $parent ('Colposcopy-stage-'+[guid]::NewGuid().ToString())
$backup=Join-Path $parent ('Colposcopy-backup-'+[guid]::NewGuid().ToString())
New-Item -ItemType Directory $stage | Out-Null
try {
 foreach ($file in $manifest.files) {
  $target=Join-Path $stage $file.path
  New-Item -ItemType Directory -Force (Split-Path -Parent $target) | Out-Null
  Copy-Item -LiteralPath (Join-Path $source $file.path) -Destination $target
  if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -cne $file.sha256) {throw 'Staging integrity failed'}
 }
 Copy-Item -LiteralPath (Join-Path $source 'package-manifest.json') -Destination $stage
 if (Test-Path -LiteralPath $destination) {Move-Item -LiteralPath $destination -Destination $backup}
 try {Move-Item -LiteralPath $stage -Destination $destination} catch {
  if (Test-Path -LiteralPath $backup) {Move-Item -LiteralPath $backup -Destination $destination}
  throw
 }
 $shell=New-Object -ComObject WScript.Shell
 $shortcut=$shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'Colposcopy.lnk'))
 $shortcut.TargetPath=Join-Path $destination 'Colposcopy.exe';$shortcut.WorkingDirectory=$destination;$shortcut.Save()
 if (Test-Path -LiteralPath $backup) {Remove-Item -LiteralPath $backup -Recurse -Force}
 Write-Output "Installed: $destination"
} finally {if (Test-Path -LiteralPath $stage) {Remove-Item -LiteralPath $stage -Recurse -Force}}
