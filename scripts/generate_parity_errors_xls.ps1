# Generates tests/fixtures/parity_errors.xls, the legacy-BIFF twin of the
# parity-errors workbook that test_gaps_parity_errors.py authors as .xlsx.
# The layout matches HEADER_ROWS / GOLDEN in that test:
#   header row:  a | b | c
#   data row 0:  1 | #DIV/0! | 3
#   data row 1:  4 | 5 | #N/A
#   data row 2:  #VALUE! | 7 | 8
#   data row 3:  9 | #NAME? | 11
# Formulas are used so Excel computes and caches the typed error values; the
# workbook is saved as Excel 97-2003 (.xls, FileFormat 56 / xlExcel8).
# Requires Excel COM (Windows only).

$ErrorActionPreference = "Stop"

$outPath = Join-Path $PSScriptRoot "..\tests\fixtures\parity_errors.xls"
$outPath = [System.IO.Path]::GetFullPath($outPath)

$excel = New-Object -ComObject Excel.Application
try {
    $excel.Visible = $false
    $excel.DisplayAlerts = $false

    $wb = $excel.Workbooks.Add()
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = "Sheet1"

    # Header
    $ws.Range("A1").Value2 = "a"
    $ws.Range("B1").Value2 = "b"
    $ws.Range("C1").Value2 = "c"

    # Data rows (formulas -> Excel caches the error result)
    $ws.Range("A2").Value2 = 1
    $ws.Range("B2").Formula = "=1/0"      # #DIV/0!
    $ws.Range("C2").Value2 = 3

    $ws.Range("A3").Value2 = 4
    $ws.Range("B3").Value2 = 5
    $ws.Range("C3").Formula = "=NA()"     # #N/A

    $ws.Range("A4").Formula = '=VALUE("x")'  # #VALUE!
    $ws.Range("B4").Value2 = 7
    $ws.Range("C4").Value2 = 8

    $ws.Range("A5").Value2 = 9
    $ws.Range("B5").Formula = "=FOOBAR()" # #NAME?
    $ws.Range("C5").Value2 = 11

    $wb.Application.CalculateFull()

    $wb.SaveAs($outPath, 56) # 56 = xlExcel8 (.xls)
    $wb.Close($false)

    Write-Output "WROTE $outPath"
}
finally {
    $excel.Quit()
    [System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
}