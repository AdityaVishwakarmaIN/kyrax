"""A5 architecture-stress Wave-1 baseline (Python reachability).

Wave-1 is harness-independent and runs ONLY clean, non-hostile inputs. Every
malformed/hostile/COM case is deferred until the A6 exact-PID timeout/RSS
coordinator and the shared harness (common.py / fixtures.py / test_metrics.py)
exist; nothing here imports them.

Evidence-based reachability: the tests introspect the installed ``kyrax`` module
for the documented binding surface instead of assuming it, build one minimal
clean workbook with the standard library, and exercise validate/repair/read.
Bindings that are absent are recorded as KNOWN-GAP markers, not asserted.

Run with pytest from the repo root:
    pytest tests_turbo/architecture_stress/test_resilience.py
"""

from __future__ import annotations

import os
import zipfile
from pathlib import Path

import pytest

import kyrax

EXPECTED_BINDINGS = [
    "validate_excel",
    "repair_excel",
    "load_workbook",
    "read_excel",
]

# CSV/JSON/stream-read bindings are NOT part of the documented public surface
# as of Wave-1; the plan marks them KNOWN-GAP if unbound.
EXPECTED_KNOWN_GAP = [
    "sheet_to_csv",
    "csv_to_sheet",
    "sheet_to_json",
    "json_to_sheet",
    "SheetStream",
]


def _minimal_clean_xlsx(path: str) -> None:
    """Author a tiny, valid OOXML package with the standard library only."""
    ct = (
        '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/'
        'package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.'
        'relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-'
        'officedocument.spreadsheetml.sheet.main+xml"/>'
        '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.'
        'openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        "<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-"
        'officedocument.spreadsheetml.styles+xml"/>'
        "</Types>"
    )
    root = (
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/'
        'package/2006/relationships">'
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/'
        '2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>'
    )
    wb = (
        '<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/'
        'spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/'
        '2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/>'
        "</sheets></workbook>"
    )
    wb_rels = (
        '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/'
        'package/2006/relationships">'
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/'
        '2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
        '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/'
        '2006/relationships/styles" Target="styles.xml"/></Relationships>'
    )
    sheet = (
        '<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/'
        'spreadsheetml/2006/main"><dimension ref="A1:B2"/><sheetData>'
        '<row r="1"><c r="A1" t="inlineStr"><is><t>a</t></is></c>'
        '<c r="B1"><v>1</v></c></row>'
        '<row r="2"><c r="A2" t="inlineStr"><is><t>b</t></is></c>'
        '<c r="B2"><v>2.5</v></c></row>'
        "</sheetData></worksheet>"
    )
    styles = (
        '<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/'
        'spreadsheetml/2006/main"><fonts count="1"><font/></fonts>'
        '<fills count="1"><fill/></fills><borders count="1"><border/></borders>'
        '<cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>'
        "</cellXfs></styleSheet>"
    )
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", ct)
        z.writestr("_rels/.rels", root)
        z.writestr("xl/workbook.xml", wb)
        z.writestr("xl/_rels/workbook.xml.rels", wb_rels)
        z.writestr("xl/worksheets/sheet1.xml", sheet)
        z.writestr("xl/styles.xml", styles)


@pytest.fixture(scope="module")
def clean_xlsx(tmp_path_factory):
    path = tmp_path_factory.mktemp("a5") / "clean.xlsx"
    _minimal_clean_xlsx(str(path))
    return str(path)


def test_binding_surface_is_evidence_based():
    """Emit deterministic IO-03 evidence for coordinator classification.

    A successful pytest assertion means only that the capability probe ran and
    reported every expected symbol. It does not convert an absent binding into
    PASS: the coordinator records each ``False`` value as ``KNOWN-GAP`` until
    the A6 result helper is available.
    """
    present = [name for name in EXPECTED_BINDINGS if hasattr(kyrax, name)]
    missing_core = [name for name in EXPECTED_BINDINGS if not hasattr(kyrax, name)]
    print(f"A5 IO-03 reachability: core present={present} missing={missing_core}")
    assert not missing_core, f"core read/validate bindings missing: {missing_core}"

    evidence = {name: hasattr(kyrax, name) for name in EXPECTED_KNOWN_GAP}
    print(f"A5 IO-03 CAPABILITY_EVIDENCE={evidence}")
    for name, reachable in evidence.items():
        if not reachable:
            print(f"A5 IO-03 KNOWN-GAP: binding '{name}' is unbound in kyrax")
    assert list(evidence) == EXPECTED_KNOWN_GAP


def test_validate_excel_clean_no_false_positives(clean_xlsx):
    assert hasattr(kyrax, "validate_excel"), "documented validate_excel binding missing"
    result = kyrax.validate_excel(clean_xlsx)
    print(f"A5 VAL-01 python evidence: {result}")
    findings = result.get("findings", []) if isinstance(result, dict) else []
    errors = [f for f in findings if f.get("severity") == "error"]
    warnings = [f for f in findings if f.get("severity") == "warning"]
    assert not errors, f"A5 VAL-01: false-positive errors on a clean file: {errors}"
    assert not warnings, f"A5 VAL-01: false-positive warnings on a clean file: {warnings}"


def test_read_excel_reachability(clean_xlsx):
    assert hasattr(kyrax, "read_excel"), "documented read_excel binding missing"
    reader = kyrax.read_excel(clean_xlsx)
    names = reader.sheet_names
    print(f"A5 IO-03 read_excel evidence: sheet_names={names}")
    assert "Sheet1" in names, f"expected Sheet1, got {names}"


def test_repair_excel_clean_is_idempotent(clean_xlsx, tmp_path):
    assert hasattr(kyrax, "repair_excel"), "documented repair_excel binding missing"
    out = str(tmp_path / "repaired.xlsx")
    first = kyrax.repair_excel(clean_xlsx, out)
    print(f"A5 REP-01 python evidence (first): {first}")
    assert first.get("wrote_output") and os.path.exists(out)
    out2 = str(tmp_path / "repaired2.xlsx")
    second = kyrax.repair_excel(out, out2)
    print(f"A5 REP-01 python evidence (second): {second}")
    assert second.get("wrote_output") and os.path.exists(out2)
    assert Path(out).read_bytes() == Path(out2).read_bytes()
