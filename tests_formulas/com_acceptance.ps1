param(
    [string]$WorkbookDir = ""
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $WorkbookDir) {
    $WorkbookDir = Join-Path $root "formula-validation\round2\com_acceptance"
}
$evidence = Join-Path $root "formula-validation\round2\com_acceptance.txt"
$files = @(Get-ChildItem -LiteralPath $WorkbookDir -Filter "*.xlsx" -File | Sort-Object Name)
if ($files.Count -eq 0) {
    throw "No acceptance workbooks found in $WorkbookDir"
}

$excel = $null
$results = @()
$failed = 0
try {
    $excel = New-Object -ComObject Excel.Application
    $excel.Visible = $false
    $excel.DisplayAlerts = $false
    $excel.ScreenUpdating = $false
    $excel.AskToUpdateLinks = $false
    try { $excel.AutomationSecurity = 3 } catch {}

    foreach ($file in $files) {
        $wb = $null
        try {
            # CorruptLoad defaults to xlNormalLoad: Excel must open the package
            # normally, not through its repair or data-extraction modes.
            $wb = $excel.Workbooks.Open($file.FullName, 0, $true)
            if ($wb.Worksheets.Count -lt 1) {
                throw "workbook has no worksheets"
            }
            $excel.CalculateFullRebuild()
            $results += "PASS | $($file.Name) | sheets=$($wb.Worksheets.Count) | format=$($wb.FileFormat)"
        } catch {
            $failed++
            $results += "FAIL | $($file.Name) | $($_.Exception.Message)"
        } finally {
            if ($null -ne $wb) {
                $wb.Close($false)
                [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($wb)
            }
        }
    }

    $header = @(
        "# Formula Green Excel COM acceptance"
        "# timestamp | $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
        "# excel | version=$($excel.Version) | build=$($excel.Build)"
        "# mode | xlNormalLoad; read-only; DisplayAlerts=false; full recalculation"
        "# total | $($files.Count) | failed=$failed"
    )
    @($header + $results) | Out-File -LiteralPath $evidence -Encoding utf8
    $results
    "TOTAL=$($files.Count) FAILED=$failed"
} finally {
    if ($null -ne $excel) {
        $excel.Quit()
        [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($excel)
    }
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
}

if ($failed -ne 0) {
    exit 1
}
