param([string]$SourcesDirectory = '')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is required' }

$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'print-runtime-manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if ($manifest.format -ne 1 -or $manifest.runtime_id -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$' -or @($manifest.files).Count -eq 0 -or @($manifest.files).Count -gt 1000) {
    throw 'Invalid print runtime manifest'
}
$seen = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
$total = 0L
foreach ($file in $manifest.files) {
    if ($file.path -notmatch '^(chrome|pdfjs)/[A-Za-z0-9._/-]+$' -or $file.path -match '(^|/)\.\.?(/|$)|//|/$' -or
        -not $seen.Add([string]$file.path) -or $file.size -le 0 -or $file.sha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Invalid print runtime file'
    }
    $total += [long]$file.size
}
if ($total -gt 512MB) { throw 'Print runtime exceeds the size limit' }

function Test-PinnedFile([string]$Path, [long]$Size, [string]$Sha256) {
    $item = Get-Item -LiteralPath $Path -ErrorAction SilentlyContinue
    return ($null -ne $item -and -not $item.PSIsContainer -and $item.Length -eq $Size -and
        (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ieq $Sha256)
}

function Test-PrintRuntime([string]$Directory) {
    foreach ($file in $manifest.files) {
        if (-not (Test-PinnedFile (Join-Path $Directory $file.path) $file.size $file.sha256)) { return $false }
    }
    return $true
}

$runtimeRoot = Join-Path $env:LOCALAPPDATA 'Colposcopy/print-runtime'
$destination = Join-Path $runtimeRoot $manifest.runtime_id
if (Test-PrintRuntime $destination) { return }

$upstream = @{
    'chrome-headless-shell-win64.zip' = '^https://storage\.googleapis\.com/chrome-for-testing-public/[0-9.]+/win64/chrome-headless-shell-win64\.zip$'
    'pdfjs-6.4.299-dist.zip' = '^https://github\.com/mozilla/pdf\.js/releases/download/v6\.4\.299/pdfjs-6\.4\.299-dist\.zip$'
}
if (@($manifest.sources).Count -ne $upstream.Count) { throw 'Invalid print runtime sources' }
New-Item -ItemType Directory -Path $runtimeRoot -Force | Out-Null
$work = Join-Path $runtimeRoot ('setup-' + [guid]::NewGuid())
$stage = Join-Path $work 'verified'
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archives = @{}
try {
    foreach ($source in $manifest.sources) {
        $pattern = $upstream[$source.name]
        if (-not $pattern -or $source.url -cnotmatch $pattern -or $archives.ContainsKey($source.name) -or
            $source.size -le 0 -or $source.size -gt 256MB -or $source.sha256 -cnotmatch '^[0-9a-f]{64}$') {
            throw 'Unsupported print runtime source'
        }
        if ($SourcesDirectory) {
            $download = Join-Path $SourcesDirectory $source.name
        } else {
            $download = Join-Path $work $source.name
            Invoke-WebRequest -UseBasicParsing -Uri $source.url -OutFile $download -TimeoutSec 180
        }
        if (-not (Test-PinnedFile $download $source.size $source.sha256)) { throw "Print runtime download mismatch: $($source.name)" }
        $archives[$source.name] = [IO.Compression.ZipFile]::OpenRead($download)
    }
    $chrome = $archives['chrome-headless-shell-win64.zip']
    $pdfjs = $archives['pdfjs-6.4.299-dist.zip']
    foreach ($file in $manifest.files) {
        if ($file.path.StartsWith('chrome/')) {
            $entry = $chrome.GetEntry('chrome-headless-shell-win64/' + $file.path.Substring(7))
        } else {
            $leaf = $file.path.Substring(6)
            $entry = if ($leaf -ceq 'LICENSE') { $pdfjs.GetEntry($leaf) } else { $pdfjs.GetEntry('build/' + $leaf) }
        }
        if ($null -eq $entry -or $entry.Length -ne $file.size) { throw "Print runtime entry mismatch: $($file.path)" }
        $target = Join-Path $stage $file.path
        New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
        $entryStream = $entry.Open()
        try {
            $output = [IO.File]::Open($target, [IO.FileMode]::CreateNew)
            try { $entryStream.CopyTo($output) } finally { $output.Dispose() }
        } finally { $entryStream.Dispose() }
        if (-not (Test-PinnedFile $target $file.size $file.sha256)) { throw "Print runtime entry hash mismatch: $($file.path)" }
    }
    $lockPath = Join-Path $runtimeRoot ($manifest.runtime_id + '.install.lock')
    $lock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    try {
        if (Test-PrintRuntime $destination) { return }
        if (Test-Path -LiteralPath $destination) {
            Move-Item -LiteralPath $destination -Destination (Join-Path $runtimeRoot ('quarantine-' + [guid]::NewGuid()))
        }
        Move-Item -LiteralPath $stage -Destination $destination
    } finally { $lock.Dispose() }
} finally {
    foreach ($zip in $archives.Values) { $zip.Dispose() }
    if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
}
Write-Output 'Print components verified and installed'
