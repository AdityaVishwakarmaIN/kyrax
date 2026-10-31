"""Validate a workbook, create a repairable defect, and repair it.

Run from any directory; the example writes ``validate_*`` files there.
"""

import zipfile

import kyrax

SOURCE = "validate_source.xlsx"
BROKEN = "validate_broken.xlsx"
REPAIRED = "validate_repaired.xlsx"

kyrax.write_excel_turbo(
    SOURCE,
    [{"name": "Data", "rows": [["Name", "Count"], ["Apple", 2]]}],
)
print("Source valid:", kyrax.validate_excel(SOURCE)["valid"])

with zipfile.ZipFile(SOURCE) as source, zipfile.ZipFile(BROKEN, "w") as broken:
    for item in source.infolist():
        if item.filename == "docProps/core.xml":
            continue
        broken.writestr(item, source.read(item.filename))

report = kyrax.validate_excel(BROKEN)
print("Broken valid:", report["valid"])
for finding in report["findings"]:
    print(
        " ",
        finding["severity"],
        finding["code"],
        "repairable=" + str(finding["repairable"]),
    )

result = kyrax.repair_excel(BROKEN, REPAIRED)
print("Wrote repaired output:", result["wrote_output"])
print("Repair actions:", [action["code"] for action in result["actions"]])
print("Repaired valid:", kyrax.validate_excel(REPAIRED)["valid"])
