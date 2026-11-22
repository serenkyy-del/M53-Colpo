param(
 [string]$AppExe='',
 [Parameter(Mandatory=$true)][string]$RuntimeArchive,
 [Parameter(Mandatory=$true)][string]$RustLicensesDirectory,
 [string]$OutputDirectory='',
 [string]$VCRuntimeDll=''
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$root=Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $OutputDirectory) {$OutputDirectory=Join-Path $PSScriptRoot 'dist'}
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
if (-not $AppExe) {
 Push-Location $root
 try {
  cargo build --manifest-path prototypes/gpui-editor/Cargo.toml --target x86_64-pc-windows-msvc --profile distribution --features standalone --bin m53-gpui-editor --locked
  if ($LASTEXITCODE -ne 0) {throw 'Native MSVC build failed'}
 } finally {Pop-Location}
 $AppExe=Join-Path $root 'prototypes/gpui-editor/target/x86_64-pc-windows-msvc/distribution/m53-gpui-editor.exe'
}
foreach ($path in @($AppExe,$RuntimeArchive)) {if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {throw "Missing input: $path"}}
if (-not (Test-Path -LiteralPath $RustLicensesDirectory -PathType Container) -or @(Get-ChildItem -LiteralPath $RustLicensesDirectory -Recurse -File).Count -eq 0) {throw 'Provide complete dependency license notices for this Cargo.lock'}
# The compiler embeds this immutable manifest. Never rebuild or replace its trust anchor.
$runtimeManifest=Join-Path $root 'assets/print-runtime/manifest.json'
$runtime=Get-Content -LiteralPath $runtimeManifest -Raw | ConvertFrom-Json
if ((Get-Item -LiteralPath $RuntimeArchive).Length -ne $runtime.archive_size -or (Get-FileHash -LiteralPath $RuntimeArchive -Algorithm SHA256).Hash.ToLowerInvariant() -cne $runtime.archive_sha256) {throw 'Pinned print runtime archive fingerprint mismatch'}
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip=[IO.Compression.ZipFile]::OpenRead([IO.Path]::GetFullPath($RuntimeArchive))
try {
 if ($zip.Entries.Count -ne @($runtime.files).Count) {throw 'Runtime entry count mismatch'}
 foreach ($file in $runtime.files) {
  $entry=$zip.GetEntry($file.path)
  if ($null -eq $entry -or $entry.Length -ne $file.size) {throw 'Runtime entry mismatch'}
  $stream=$entry.Open();$sha=[Security.Cryptography.SHA256]::Create()
  try {$hash=[BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-','').ToLowerInvariant()} finally {$stream.Dispose();$sha.Dispose()}
  if ($hash -cne $file.sha256) {throw 'Runtime file hash mismatch'}
 }
} finally {$zip.Dispose()}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$archive=Join-Path $OutputDirectory 'Colposcopy-Windows-x64.zip'
if (Test-Path -LiteralPath $archive) {throw 'Output exists; choose a new output directory'}
$stage=Join-Path $OutputDirectory ('stage-'+[guid]::NewGuid().ToString())
New-Item -ItemType Directory $stage | Out-Null
try {
 Copy-Item -LiteralPath $AppExe -Destination (Join-Path $stage 'Colposcopy.exe')
 Copy-Item -LiteralPath $RuntimeArchive -Destination (Join-Path $stage 'm53-print-runtime.zip')
 Copy-Item -LiteralPath $runtimeManifest -Destination (Join-Path $stage 'print-runtime-manifest.json')
 Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'Setup.ps1') -Destination $stage
 Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'README.txt') -Destination $stage
 $licenses=Join-Path $stage 'licenses';New-Item -ItemType Directory $licenses | Out-Null
 Copy-Item -LiteralPath (Join-Path $root 'crates/client/assets/FONT-LICENSE.txt') -Destination $licenses
 $sourceLicense=Join-Path $root 'LICENSE'
 if (-not (Test-Path -LiteralPath $sourceLicense -PathType Leaf)) {$sourceLicense=Join-Path $root 'public/LICENSE'}
 Copy-Item -LiteralPath $sourceLicense -Destination $licenses
 Copy-Item -LiteralPath $RustLicensesDirectory -Destination (Join-Path $licenses 'rust') -Recurse
 if ($VCRuntimeDll) {Copy-Item -LiteralPath $VCRuntimeDll -Destination (Join-Path $stage 'vcruntime140.dll')}
 $records=@(Get-ChildItem -LiteralPath $stage -Recurse -File | Sort-Object FullName | ForEach-Object {
  [ordered]@{path=$_.FullName.Substring($stage.Length+1).Replace('\','/');size=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
 })
 [IO.File]::WriteAllText((Join-Path $stage 'package-manifest.json'),(@{format=1;files=$records} | ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
 [IO.Compression.ZipFile]::CreateFromDirectory([IO.Path]::GetFullPath($stage),[IO.Path]::GetFullPath($archive))
 $hash=(Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
 [IO.File]::WriteAllText(($archive+'.sha256'),($hash+'  Colposcopy-Windows-x64.zip'+[Environment]::NewLine),[Text.UTF8Encoding]::new($false))
 Write-Output $archive
} finally {if (Test-Path -LiteralPath $stage) {Remove-Item -LiteralPath $stage -Recurse -Force}}
