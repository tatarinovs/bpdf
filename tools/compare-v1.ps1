param(
    [Parameter(Mandatory = $true)]
    [string]$ReferenceExe,

    [Parameter(Mandatory = $true)]
    [string]$CandidateExe,

    [string]$Config,

    [string]$WorkDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$reference = [IO.Path]::GetFullPath($ReferenceExe)
$candidate = [IO.Path]::GetFullPath($CandidateExe)
if ([string]::IsNullOrWhiteSpace($Config)) {
    $Config = Join-Path $PSScriptRoot '..\config.example.jsonc'
}
$configPath = [IO.Path]::GetFullPath($Config)
foreach ($path in @($reference, $candidate, $configPath)) {
    if (-not [IO.File]::Exists($path)) {
        throw "Required file does not exist: $path"
    }
}

if ([string]::IsNullOrWhiteSpace($WorkDir)) {
    $WorkDir = Join-Path ([IO.Path]::GetTempPath()) ("bpdf-parity-" + [guid]::NewGuid().ToString('N'))
}
$root = [IO.Path]::GetFullPath($WorkDir)
[IO.Directory]::CreateDirectory($root) | Out-Null

function Invoke-Bpdf([string]$Exe, [string[]]$Arguments) {
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Exe failed with exit code ${LASTEXITCODE}: $($Arguments -join ' ')"
    }
}

function Assert-SameFile([string]$Left, [string]$Right, [string]$Label) {
    $leftHash = (Get-FileHash -LiteralPath $Left -Algorithm SHA256).Hash
    $rightHash = (Get-FileHash -LiteralPath $Right -Algorithm SHA256).Hash
    if ($leftHash -ne $rightHash) {
        throw "$Label differs: $leftHash != $rightHash"
    }
    Write-Output "PASS $Label $leftHash"
}

Add-Type -AssemblyName System.Drawing
$bitmap = [Drawing.Bitmap]::new(1200, 800)
$graphics = [Drawing.Graphics]::FromImage($bitmap)
try {
    $graphics.Clear([Drawing.Color]::White)
    $graphics.FillRectangle([Drawing.Brushes]::DarkRed, 50, 50, 500, 300)
    $font = [Drawing.Font]::new('Arial', 48)
    try {
        $graphics.DrawString('bpdf parity', $font, [Drawing.Brushes]::Black, 100, 400)
    } finally {
        $font.Dispose()
    }
    $image = Join-Path $root 'sample.png'
    $bitmap.Save($image, [Drawing.Imaging.ImageFormat]::Png)
} finally {
    $graphics.Dispose()
    $bitmap.Dispose()
}

function Get-HelpOptions([string]$Exe, [string[]]$Arguments) {
    $helpText = (& $Exe @Arguments 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "$Exe failed to show help: $($Arguments -join ' ')"
    }

    return @(
        [regex]::Matches($helpText, '(?<![\w-])--[a-z][a-z0-9-]*|(?<![\w-])-[A-Za-z](?=[,\s])') |
            ForEach-Object { $_.Value } |
            Sort-Object -Unique
    )
}

$helpCommands = @(
    @('--help'), @('merge', '--help'), @('ocr', '--help'), @('split', '--help'),
    @('extract', '--help'), @('inspect', '--help'), @('strip', '--help'),
    @('rotate', '--help'), @('resize', '--help'), @('text', '--help'),
    @('doctor', '--help'), @('stamp', '--help'), @('optimize', '--help'),
    @('metadata', '--help'), @('metadata', 'show', '--help'),
    @('metadata', 'set', '--help'), @('convert', '--help')
)
foreach ($arguments in $helpCommands) {
    $oldOptions = @(Get-HelpOptions $reference $arguments)
    $newOptions = @(Get-HelpOptions $candidate $arguments)
    $missing = @($oldOptions | Where-Object { $_ -notin $newOptions })
    if ($missing.Count -gt 0) {
        throw "CLI options missing for '$($arguments -join ' ')': $($missing -join ', ')"
    }
}
Write-Output 'PASS CLI option compatibility'

$oldPdf = Join-Path $root 'old.pdf'
$newPdf = Join-Path $root 'new.pdf'
Invoke-Bpdf $reference @('--config', $configPath, '--quiet', 'merge', $image, $image, '-o', $oldPdf, '--size', 'A4')
Invoke-Bpdf $candidate @('--config', $configPath, '--quiet', 'merge', $image, $image, '-o', $newPdf, '--size', 'A4')
Assert-SameFile $oldPdf $newPdf 'merge'

$cases = @(
    @{ Name = 'optimize'; Args = @('optimize', $oldPdf) },
    @{ Name = 'rotate'; Args = @('rotate', $oldPdf, '90') },
    @{ Name = 'resize'; Args = @('resize', $oldPdf, '--size', 'Letter') },
    @{ Name = 'metadata'; Args = @('metadata', 'set', $oldPdf, '--title', 'Parity') },
    @{ Name = 'strip'; Args = @('strip', $oldPdf) }
)
foreach ($case in $cases) {
    $oldOutput = Join-Path $root ("old-$($case.Name).pdf")
    $newOutput = Join-Path $root ("new-$($case.Name).pdf")
    $oldArgs = @('--config', $configPath, '--quiet') + $case.Args + @('-o', $oldOutput)
    $newArgs = @('--config', $configPath, '--quiet') + $case.Args + @('-o', $newOutput)
    Invoke-Bpdf $reference $oldArgs
    Invoke-Bpdf $candidate $newArgs
    Assert-SameFile $oldOutput $newOutput $case.Name
}

$oldConvert = Join-Path $root 'old-convert'
$newConvert = Join-Path $root 'new-convert'
Invoke-Bpdf $reference @('--config', $configPath, '--quiet', 'convert', $image, '-o', $oldConvert)
Invoke-Bpdf $candidate @('--config', $configPath, '--quiet', 'convert', $image, '-o', $newConvert)
Assert-SameFile (Join-Path $oldConvert 'sample.jpg') (Join-Path $newConvert 'sample.jpg') 'convert'

$oldSplit = Join-Path $root 'old-split'
$newSplit = Join-Path $root 'new-split'
Invoke-Bpdf $reference @('--config', $configPath, '--quiet', 'split', $oldPdf, $oldSplit)
Invoke-Bpdf $candidate @('--config', $configPath, '--quiet', 'split', $oldPdf, $newSplit)
$oldPages = @(Get-ChildItem -LiteralPath $oldSplit -Filter *.pdf | Sort-Object Name)
$newPages = @(Get-ChildItem -LiteralPath $newSplit -Filter *.pdf | Sort-Object Name)
if ($oldPages.Count -ne $newPages.Count) {
    throw "split page count differs: $($oldPages.Count) != $($newPages.Count)"
}
for ($index = 0; $index -lt $oldPages.Count; $index++) {
    Assert-SameFile $oldPages[$index].FullName $newPages[$index].FullName "split page $($index + 1)"
}

Write-Output "Parity corpus: $root"
