[CmdletBinding()]
param(
    [ValidateSet('Quick', 'Manifest', 'Selected')]
    [string]$Mode = 'Quick',
    [int]$Seed = 42,
    [string]$OutputDir,
    [string[]]$TestId = @()
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$python = Join-Path $repoRoot '.venv\Scripts\python.exe'
$metrics = Join-Path $repoRoot 'tests_turbo\architecture_stress\test_metrics.py'

if (-not (Test-Path -LiteralPath $python -PathType Leaf)) {
    throw "Expected virtual-environment Python is missing: $python"
}

if (-not $OutputDir) {
    $stamp = Get-Date -Format 'yyyyMMdd_HHmmss'
    $OutputDir = Join-Path $repoRoot "architecture_stress_runs\${stamp}_seed${Seed}"
}
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
[void](New-Item -ItemType Directory -Path $OutputDir -Force)

$env:KYRAX_ARCHSTRESS_SEED = [string]$Seed
$env:KYRAX_ARCHSTRESS_OUTPUT = $OutputDir
$started = [DateTimeOffset]::UtcNow
$exitCode = 0
$command = @()

Push-Location $repoRoot
try {
    switch ($Mode) {
        'Quick' {
            $command = @(
                '-m', 'pytest', $metrics, '-q',
                '-m', 'not large and not excel_com and not northstar'
            )
            $savedPreference = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try {
                & $python @command 2>&1 |
                    Tee-Object -FilePath (Join-Path $OutputDir 'quick.log')
                $exitCode = $LASTEXITCODE
            }
            finally {
                $ErrorActionPreference = $savedPreference
            }
        }
        'Selected' {
            if ($TestId.Count -eq 0) {
                throw 'Selected mode requires at least one -TestId value.'
            }
            $expression = $TestId -join ' or '
            $command = @('-m', 'pytest', $metrics, '-q', '-k', $expression)
            $savedPreference = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try {
                & $python @command 2>&1 |
                    Tee-Object -FilePath (Join-Path $OutputDir 'selected.log')
                $exitCode = $LASTEXITCODE
            }
            finally {
                $ErrorActionPreference = $savedPreference
            }
        }
        'Manifest' {
            $manifestCode = @'
import dataclasses
import os
import sys
from pathlib import Path

root = Path.cwd()
sys.path.insert(0, str(root / 'tests_turbo' / 'architecture_stress'))
import fixtures

seed = int(os.environ['KYRAX_ARCHSTRESS_SEED'])
output = Path(os.environ['KYRAX_ARCHSTRESS_OUTPUT']) / 'fixture_manifest.json'
registry = fixtures.FixtureRegistry()
for spec in fixtures.canonical_specs():
    registry.register(dataclasses.replace(spec, seed=seed))
registry.export(output, allow_incomplete=True)
print(output)
'@
            $command = @('-c', $manifestCode)
            $savedPreference = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try {
                & $python @command 2>&1 |
                    Tee-Object -FilePath (Join-Path $OutputDir 'manifest.log')
                $exitCode = $LASTEXITCODE
            }
            finally {
                $ErrorActionPreference = $savedPreference
            }
        }
    }
}
finally {
    Pop-Location
    $finished = [DateTimeOffset]::UtcNow
    $summary = [ordered]@{
        schema_version = 1
        mode = $Mode
        seed = $Seed
        output_dir = $OutputDir
        command = @($python) + $command
        started_utc = $started.ToString('o')
        finished_utc = $finished.ToString('o')
        duration_seconds = [Math]::Round(($finished - $started).TotalSeconds, 3)
        exit_code = $exitCode
        large_generators_implemented = $false
        note = 'Canonical F01 is pinned and verified; F02-F12 remain blocked until real artifacts and measured fields exist.'
    }
    $summaryPath = Join-Path $OutputDir 'run_summary.json'
    $summary | ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath $summaryPath -Encoding utf8
    Write-Host "Evidence summary: $summaryPath"
}

exit $exitCode
