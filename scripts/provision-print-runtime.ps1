param([string]$OutputDirectory='tmp/print-runtime-bundle')
$ErrorActionPreference='Stop'
$root=Split-Path -Parent $PSScriptRoot
$download=Join-Path $root 'tmp/print-runtime'
New-Item -ItemType Directory -Force $download | Out-Null
$sources=@(
    [ordered]@{name='chrome-headless-shell-win64.zip';version='156.0.8078.47';url='https://storage.googleapis.com/chrome-for-testing-public/156.0.8078.47/win64/chrome-headless-shell-win64.zip';size=126051683;sha256='e0af9b23f8051cca4f37ca49cd425e8055701bfc0383c198d67f8d5cc5884249'},
    [ordered]@{name='pdfjs-6.4.299-dist.zip';version='6.4.299';url='https://github.com/mozilla/pdf.js/releases/download/v6.4.299/pdfjs-6.4.299-dist.zip';size=6331451;sha256='f2245eb7ef3f674c872fe568a177bfec15874f5c034613e55d8e307f93ebd86d'}
)
$runtimeId='chrome-156.0.8078.47-pdfjs-6.4.299-win64'
$output=if([IO.Path]::IsPathRooted($OutputDirectory)){$OutputDirectory}else{Join-Path $root $OutputDirectory}
$archive=Join-Path $output 'm53-print-runtime.zip'
$manifestPath=Join-Path $root 'assets/print-runtime/manifest.json'
function Test-PinnedRuntimeCache {
    if (!(Test-Path -LiteralPath $archive -PathType Leaf) -or !(Test-Path -LiteralPath $manifestPath -PathType Leaf)) {return $false}
    try {
        $cached=Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        if ($cached.format -ne 1 -or $cached.runtime_id -cne $runtimeId -or $cached.archive -cne 'm53-print-runtime.zip' -or $cached.archive_sha256 -cnotmatch '^[0-9a-f]{64}$') {return $false}
        if (@($cached.sources).Count -ne $sources.Count -or @($cached.files).Count -lt 1 -or @($cached.files).Count -gt 1000) {return $false}
        foreach ($source in $sources) {
            $match=@($cached.sources | Where-Object {$_.name -ceq $source.name})
            if ($match.Count -ne 1) {return $false}
            foreach ($field in @('name','version','url','size','sha256')) {if ([string]$match[0].$field -cne [string]$source[$field]) {return $false}}
        }
        $paths=New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
        [long]$total=0
        foreach ($file in $cached.files) {
            if ($file.path -match '(^/|\\|:|(^|/)\.\.?(/|$)|//)' -or $file.path.Length -eq 0 -or $file.size -le 0 -or $file.sha256 -cnotmatch '^[0-9a-f]{64}$' -or !$paths.Add($file.path)) {return $false}
            $total += [long]$file.size
        }
        if ($total -gt 512*1024*1024 -or (Get-Item -LiteralPath $archive).Length -ne [long]$cached.archive_size) {return $false}
        return (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $cached.archive_sha256
    } catch {return $false}
}
# The repository manifest is the trust anchor. Keep its already verified immutable ZIP;
# different .NET/Deflate versions need not produce byte-identical recompressions.
if (Test-PinnedRuntimeCache) {
    $cached=Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    Write-Output (@{runtime_id=$cached.runtime_id;archive_size=$cached.archive_size;archive_sha256=$cached.archive_sha256;cached=$true} | ConvertTo-Json -Compress)
    return
}
foreach($source in $sources) {
    $path=Join-Path $download $source.name
    if (!(Test-Path -LiteralPath $path)) {Invoke-WebRequest -Uri $source.url -OutFile $path}
    if ((Get-Item -LiteralPath $path).Length -ne $source.size -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $source.sha256) {throw "Pinned runtime checksum mismatch: $($source.name)"}
}
# Extract only inside newly allocated staging; never overwrite an installed engine.
$staging=Join-Path $download ('stage-'+[guid]::NewGuid().ToString())
New-Item -ItemType Directory $staging | Out-Null
try {
Expand-Archive -LiteralPath (Join-Path $download $sources[0].name) -DestinationPath (Join-Path $staging 'chrome-source')
Expand-Archive -LiteralPath (Join-Path $download $sources[1].name) -DestinationPath (Join-Path $staging 'pdfjs-source')
$runtime=Join-Path $staging 'runtime'
New-Item -ItemType Directory -Path (Join-Path $runtime 'chrome'),(Join-Path $runtime 'pdfjs') | Out-Null
Copy-Item -Path (Join-Path $staging 'chrome-source/chrome-headless-shell-win64/*') -Destination (Join-Path $runtime 'chrome') -Recurse
foreach($name in @('pdf.mjs','pdf.worker.mjs')) {Copy-Item -LiteralPath (Join-Path $staging "pdfjs-source/build/$name") -Destination (Join-Path $runtime 'pdfjs')}
Copy-Item -LiteralPath (Join-Path $staging 'pdfjs-source/LICENSE') -Destination (Join-Path $runtime 'pdfjs')
# Sorted entries/timestamps are reproducible on the same .NET compression builder.
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
New-Item -ItemType Directory -Force $output | Out-Null
$stream=[IO.File]::Open($archive,[IO.FileMode]::Create)
$zip=[IO.Compression.ZipArchive]::new($stream,[IO.Compression.ZipArchiveMode]::Create,$false)
$files=@()
try {
    foreach($file in (Get-ChildItem -LiteralPath $runtime -Recurse -File | Sort-Object FullName)) {
        $relative=$file.FullName.Substring($runtime.Length+1).Replace('\','/')
        $entry=$zip.CreateEntry($relative,[IO.Compression.CompressionLevel]::Optimal)
        $entry.LastWriteTime=[DateTimeOffset]'2026-10-21T00:00:00Z'
        $sourceStream=[IO.File]::OpenRead($file.FullName);$entryStream=$entry.Open()
        try {$sourceStream.CopyTo($entryStream)} finally {$entryStream.Dispose();$sourceStream.Dispose()}
        $files+=[ordered]@{path=$relative;size=$file.Length;sha256=(Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
    }
} finally {$zip.Dispose();$stream.Dispose()}
$manifest=[ordered]@{format=1;runtime_id=$runtimeId;archive='m53-print-runtime.zip';archive_size=(Get-Item -LiteralPath $archive).Length;archive_sha256=(Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant();sources=$sources;files=$files}
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
Write-Output ([pscustomobject]$manifest | Select-Object runtime_id,archive_size,archive_sha256 | ConvertTo-Json -Compress)

} finally {
    $resolvedStage=[IO.Path]::GetFullPath($staging)
    $resolvedDownload=[IO.Path]::GetFullPath($download)
    if (!(Split-Path -Parent $resolvedStage).Equals($resolvedDownload,[StringComparison]::OrdinalIgnoreCase) -or (Split-Path -Leaf $resolvedStage) -notmatch '^stage-[0-9a-f-]{36}$') {throw 'Runtime staging cleanup path rejected'}
    if (Test-Path -LiteralPath $resolvedStage) {Remove-Item -LiteralPath $resolvedStage -Recurse -Force}
}
