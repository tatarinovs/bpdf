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

$icoDir = Join-Path $projectRoot 'ico'
$hasIcons = (Test-Path $icoDir) -and ((Get-ChildItem $icoDir -Filter *.ico).Count -ge 14)

if (-not $hasIcons) {
    Write-Output "Generating icon assets..."
    & python "$projectRoot\tools\draw_icons.py"
}

Write-Output "Prepared icon and version resources for bpdf $version"
