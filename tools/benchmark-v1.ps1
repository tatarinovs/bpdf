param(
    [Parameter(Mandatory = $true)]
    [string]$ReferenceExe,

    [Parameter(Mandatory = $true)]
    [string]$CandidateExe,

    [Parameter(Mandatory = $true)]
    [string]$Pdf,

    [ValidateRange(1, 20)]
    [int]$Runs = 3
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-Split([string]$Exe, [string]$Label) {
    $output = Join-Path ([IO.Path]::GetTempPath()) ("bpdf-bench-" + [guid]::NewGuid().ToString('N'))
    try {
        $watch = [Diagnostics.Stopwatch]::StartNew()
        & $Exe --quiet split $Pdf $output
        $watch.Stop()
        if ($LASTEXITCODE -ne 0) {
            throw "$Label failed with exit code $LASTEXITCODE"
        }
        $watch.Elapsed.TotalMilliseconds
    } finally {
        if ([IO.Directory]::Exists($output)) {
            Remove-Item -LiteralPath $output -Recurse -Force
        }
    }
}

$executables = @{
    reference = [IO.Path]::GetFullPath($ReferenceExe)
    candidate = [IO.Path]::GetFullPath($CandidateExe)
}
$samples = @{
    reference = @()
    candidate = @()
}

# Warm both executables and the input file before collecting samples.
Invoke-Split $executables.reference 'reference warmup' | Out-Null
Invoke-Split $executables.candidate 'candidate warmup' | Out-Null

for ($run = 1; $run -le $Runs; $run++) {
    $order = if (($run % 2) -eq 1) { @('reference', 'candidate') } else { @('candidate', 'reference') }
    foreach ($label in $order) {
        $samples[$label] += Invoke-Split $executables[$label] $label
    }
}

@('reference', 'candidate') | ForEach-Object {
    $measurement = $samples[$_]
    [pscustomobject]@{
        Name = $_
        Runs = $Runs
        MinimumMs = [math]::Round(($measurement | Measure-Object -Minimum).Minimum, 2)
        AverageMs = [math]::Round(($measurement | Measure-Object -Average).Average, 2)
    }
} | Format-Table -AutoSize
