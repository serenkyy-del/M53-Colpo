param(
 [string]$AppExe='',
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
if (-not (Test-Path -LiteralPath $AppExe -PathType Leaf)) {throw "Missing input: $AppExe"}
if (-not (Test-Path -LiteralPath $RustLicensesDirectory -PathType Container) -or @(Get-ChildItem -LiteralPath $RustLicensesDirectory -Recurse -File).Count -eq 0) {throw 'Provide complete dependency license notices for this Cargo.lock'}
# The installer and the compiler use the same immutable manifest.
$runtimeManifest=Join-Path $root 'assets/print-runtime/manifest.json'
Add-Type -AssemblyName System.IO.Compression.FileSystem
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$archive=Join-Path $OutputDirectory 'Colposcopy-Windows-x64.zip'
if (Test-Path -LiteralPath $archive) {throw 'Output exists; choose a new output directory'}
$stage=Join-Path $OutputDirectory ('stage-'+[guid]::NewGuid().ToString())
New-Item -ItemType Directory $stage | Out-Null
try {
 Copy-Item -LiteralPath $AppExe -Destination (Join-Path $stage 'Colposcopy.exe')
 Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'Install-PrintRuntime.ps1') -Destination $stage
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
