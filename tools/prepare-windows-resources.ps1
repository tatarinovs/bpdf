param(
    [Parameter(Mandatory = $true)]
    [string]$ProjectRoot,

    [Parameter(Mandatory = $true)]
    [string]$OutputDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Add-Type -AssemblyName System.Drawing

$projectRoot = [IO.Path]::GetFullPath($ProjectRoot)
$outputDir = [IO.Path]::GetFullPath($OutputDir)
$resourcesDir = Join-Path $projectRoot 'resources'
[IO.Directory]::CreateDirectory($resourcesDir) | Out-Null
[IO.Directory]::CreateDirectory($outputDir) | Out-Null

$cargo = [IO.File]::ReadAllText((Join-Path $projectRoot 'Cargo.toml'))
$match = [regex]::Match(
    $cargo,
    '(?m)^\s*version\s*=\s*"(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(?:[^"]*)"\s*$'
)
if (-not $match.Success) {
    throw 'Cannot read package version from Cargo.toml'
}

$major = [int]$match.Groups['major'].Value
$minor = [int]$match.Groups['minor'].Value
$patch = [int]$match.Groups['patch'].Value
$version = "$major.$minor.$patch.0"
$versionInclude = @"
#define BPDF_VERSION_NUMERIC $major,$minor,$patch,0
#define BPDF_VERSION_STRING "$version\0"
"@
$versionInclude += [Environment]::NewLine
[IO.File]::WriteAllText(
    (Join-Path $outputDir 'version.inc'),
    $versionInclude,
    [Text.UTF8Encoding]::new($false)
)

$colors = @{
    Blue = [Drawing.Color]::FromArgb(255, 0x12, 0x39, 0xB8)
    White = [Drawing.Color]::FromArgb(255, 0xFF, 0xFF, 0xFF)
    Orange = [Drawing.Color]::FromArgb(255, 0xFF, 0x6A, 0x00)
}
$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$pngs = [Collections.Generic.List[byte[]]]::new()

function New-Point([double]$x, [double]$y, [double]$scale) {
    return [Drawing.PointF]::new(
        [single]($x * $scale),
        [single]($y * $scale)
    )
}

foreach ($size in $sizes) {
    $bitmap = [Drawing.Bitmap]::new(
        $size,
        $size,
        [Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CompositingMode = [Drawing.Drawing2D.CompositingMode]::SourceCopy
        $graphics.SmoothingMode = if ($size -le 32) {
            [Drawing.Drawing2D.SmoothingMode]::None
        } else {
            [Drawing.Drawing2D.SmoothingMode]::AntiAlias
        }
        $graphics.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::Half
        $graphics.Clear($colors.Blue)

        $scale = $size / 16.0
        $whiteBrush = [Drawing.SolidBrush]::new($colors.White)
        $blueBrush = [Drawing.SolidBrush]::new($colors.Blue)
        $orangeBrush = [Drawing.SolidBrush]::new($colors.Orange)
        try {
            $page = [Drawing.PointF[]]@(
                (New-Point 3 1 $scale),
                (New-Point 10 1 $scale),
                (New-Point 14 5 $scale),
                (New-Point 14 15 $scale),
                (New-Point 3 15 $scale)
            )
            $fold = [Drawing.PointF[]]@(
                (New-Point 10 1 $scale),
                (New-Point 10 5 $scale),
                (New-Point 14 5 $scale)
            )
            $graphics.FillPolygon($whiteBrush, $page)
            $graphics.FillPolygon($blueBrush, $fold)
            $graphics.FillRectangle($blueBrush, 5 * $scale, 7 * $scale, 6 * $scale, 2 * $scale)
            $graphics.FillRectangle($blueBrush, 5 * $scale, 10 * $scale, 5 * $scale, 2 * $scale)
            $graphics.FillRectangle($orangeBrush, 11 * $scale, 11 * $scale, 2 * $scale, 2 * $scale)
        } finally {
            $whiteBrush.Dispose()
            $blueBrush.Dispose()
            $orangeBrush.Dispose()
        }

        $stream = [IO.MemoryStream]::new()
        try {
            $bitmap.Save($stream, [Drawing.Imaging.ImageFormat]::Png)
            $pngs.Add($stream.ToArray())
            if ($size -eq 16 -or $size -eq 256) {
                [IO.File]::WriteAllBytes(
                    (Join-Path $outputDir "icon-$size.png"),
                    $stream.ToArray()
                )
            }
            if ($size -eq 256) {
                [IO.File]::WriteAllBytes(
                    (Join-Path $resourcesDir 'bpdf-icon.png'),
                    $stream.ToArray()
                )
            }
        } finally {
            $stream.Dispose()
        }
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

$iconPath = Join-Path $resourcesDir 'bpdf.ico'
$file = [IO.File]::Create($iconPath)
$writer = [IO.BinaryWriter]::new($file)
try {
    $writer.Write([uint16]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]$sizes.Count)

    $offset = 6 + (16 * $sizes.Count)
    for ($index = 0; $index -lt $sizes.Count; $index++) {
        $size = $sizes[$index]
        $bytes = $pngs[$index]
        $writer.Write([byte]$(if ($size -eq 256) { 0 } else { $size }))
        $writer.Write([byte]$(if ($size -eq 256) { 0 } else { $size }))
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]$bytes.Length)
        $writer.Write([uint32]$offset)
        $offset += $bytes.Length
    }
    foreach ($bytes in $pngs) {
        $writer.Write($bytes)
    }
} finally {
    $writer.Dispose()
    $file.Dispose()
}

Write-Output "Prepared icon and version resources for bpdf $version"
