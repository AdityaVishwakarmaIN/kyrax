"""In-process format interchange: xlsx <-> csv / json.

Thin facade over the Rust turbo io layer (``_kyrax``): there is no CSV/JSON
logic in Python (CLAUDE.md law). Everything here converts a Python argument
into the Rust binding and normalises ``Path`` values.

>>> sheet_to_csv_bytes("book.xlsx", "Data")
b'Region,Amount\\r\\nEast,100\\r\\nWest,200\\r\\n'
>>> sheet_to_json_bytes("book.xlsx", "Data", shape="records")
b'[{"Region":"East","Amount":100}]'
"""

from __future__ import annotations

from os.path import expanduser
from pathlib import Path
from typing import Literal

from . import _kyrax

JsonShape = Literal["records", "columns", "ndjson"]


def sheet_to_csv(
    path: Path | str,
    sheet: str,
    out_path: Path | str,
    *,
    delimiter: str = ",",
    quote: str = '"',
    has_header: bool = True,
    infer_types: bool = False,
    date_format: str = "yyyy-mm-dd hh:mm:ss",
) -> None:
    """Stream one worksheet to a CSV file.

    RFC 4180 output (CRLF line endings, quoted fields when needed, no BOM).
    Numbers in date-formatted cells render per ``date_format`` (Excel token
    syntax); formula cells emit their cached value; empty-string cells emit
    ``""`` (distinct from blank fields).

    :param path: Path to an ``.xlsx`` workbook.
    :param sheet: Worksheet name to export (row 1 is always the first CSV line).
    :param out_path: Destination CSV file.
    :param delimiter: Field delimiter (single ASCII character, default ``","``).
    :param quote: Quote character (single ASCII character, default ``'"'``).
    :param has_header: Whether the first record is a header line (recorded for
        re-import; emitted bytes are identical either way).
    :param infer_types: Promote numeric-looking fields on re-import. Default
        ``False`` so ``"007"`` and long account numbers never become lossy
        numbers.
    :param date_format: Excel date-format pattern (default
        ``yyyy-mm-dd hh:mm:ss``).
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    if isinstance(out_path, Path):
        out_path = expanduser(str(out_path))
    _kyrax.sheet_to_csv(
        str(path),
        sheet,
        str(out_path),
        delimiter,
        quote,
        has_header,
        infer_types,
        date_format,
    )


def sheet_to_csv_bytes(
    path: Path | str,
    sheet: str,
    *,
    delimiter: str = ",",
    quote: str = '"',
    has_header: bool = True,
    infer_types: bool = False,
    date_format: str = "yyyy-mm-dd hh:mm:ss",
) -> bytes:
    """Export one worksheet to CSV text, returned as bytes.

    Same options as :func:`sheet_to_csv`; the CSV never touches disk.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    return _kyrax.sheet_to_csv_bytes(
        str(path),
        sheet,
        delimiter,
        quote,
        has_header,
        infer_types,
        date_format,
    )


def csv_to_sheet(
    csv_path: Path | str,
    xlsx_out: Path | str,
    sheet_name: str = "Sheet1",
    *,
    delimiter: str = ",",
    quote: str = '"',
    has_header: bool = True,
    infer_types: bool = False,
) -> None:
    """Parse a CSV file and write a new single-sheet workbook.

    Every record maps to a sheet row (nothing is dropped); with
    ``infer_types`` only leading-zero-safe, precision-safe numerics are
    promoted to numbers. A leading UTF-8 BOM is consumed; CRLF, LF and bare CR
    are all accepted.

    :param csv_path: Source CSV file.
    :param xlsx_out: Destination ``.xlsx`` file.
    :param sheet_name: Name of the single worksheet to create.
    :param delimiter: Field delimiter (single ASCII character).
    :param quote: Quote character (single ASCII character).
    :param has_header: Whether the first record is written as plain text column
        names (never type-inferred).
    :param infer_types: Promote numeric-looking fields to numbers.
    """
    if isinstance(csv_path, Path):
        csv_path = expanduser(str(csv_path))
    if isinstance(xlsx_out, Path):
        xlsx_out = expanduser(str(xlsx_out))
    _kyrax.csv_to_sheet(
        str(csv_path), str(xlsx_out), sheet_name, delimiter, quote, has_header, infer_types
    )


def csv_bytes_to_sheet(
    csv_bytes: bytes,
    xlsx_out: Path | str,
    sheet_name: str = "Sheet1",
    *,
    delimiter: str = ",",
    quote: str = '"',
    has_header: bool = True,
    infer_types: bool = False,
) -> None:
    """Parse an in-memory CSV buffer and write a new single-sheet workbook.

    Same semantics as :func:`csv_to_sheet`; the CSV never touches disk.
    """
    if isinstance(xlsx_out, Path):
        xlsx_out = expanduser(str(xlsx_out))
    _kyrax.csv_bytes_to_sheet(
        csv_bytes, str(xlsx_out), sheet_name, delimiter, quote, has_header, infer_types
    )


def sheet_to_json(
    path: Path | str,
    sheet: str,
    out_path: Path | str,
    *,
    shape: JsonShape = "records",
    has_header: bool = True,
    date_format: str = "",
) -> None:
    """Stream one worksheet to a JSON/NDJSON file.

    Empty cells emit ``null``, empty-string cells emit ``""``, integers beyond
    2^53 emit as strings (fidelity, never a silent lossy double), and
    date-styled cells render per ``date_format`` (strftime tokens; empty = ISO
    8601).

    :param path: Path to an ``.xlsx`` workbook.
    :param sheet: Worksheet name to export.
    :param out_path: Destination JSON/NDJSON file.
    :param shape: ``"records"`` (row-oriented array), ``"columns"``
        (column-oriented object) or ``"ndjson"`` (one object per line).
    :param has_header: Whether the sheet's row 1 supplies JSON keys.
    :param date_format: strftime-style date format on export.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    if isinstance(out_path, Path):
        out_path = expanduser(str(out_path))
    _kyrax.sheet_to_json(str(path), sheet, str(out_path), shape, has_header, date_format)


def sheet_to_json_bytes(
    path: Path | str,
    sheet: str,
    *,
    shape: JsonShape = "records",
    has_header: bool = True,
    date_format: str = "",
) -> bytes:
    """Export one worksheet to JSON/NDJSON text, returned as bytes.

    Same options as :func:`sheet_to_json`; the document never touches disk.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    return _kyrax.sheet_to_json_bytes(str(path), sheet, shape, has_header, date_format)


def json_to_sheet(
    json_path: Path | str,
    xlsx_out: Path | str,
    sheet_name: str = "Sheet1",
    *,
    shape: JsonShape = "records",
    has_header: bool = True,
) -> None:
    """Parse a JSON/NDJSON file and write a new single-sheet workbook.

    ``Records``/``Ndjson`` accept heterogeneous keys per record; the first-seen
    key union becomes the columns with missing values as empty cells. Nested
    objects/arrays land as their raw JSON text; integers beyond 2^53 are kept
    as their digit strings.

    :param json_path: Source JSON/NDJSON file.
    :param xlsx_out: Destination ``.xlsx`` file.
    :param sheet_name: Name of the single worksheet to create.
    :param shape: ``"records"`` | ``"columns"`` | ``"ndjson"``.
    :param has_header: Accepted for API symmetry; the key union always lands in
        row 1 because a grid has no "unlabelled" state.
    """
    if isinstance(json_path, Path):
        json_path = expanduser(str(json_path))
    if isinstance(xlsx_out, Path):
        xlsx_out = expanduser(str(xlsx_out))
    _kyrax.json_to_sheet(str(json_path), str(xlsx_out), sheet_name, shape, has_header)


def json_bytes_to_sheet(
    json_bytes: bytes,
    xlsx_out: Path | str,
    sheet_name: str = "Sheet1",
    *,
    shape: JsonShape = "records",
    has_header: bool = True,
) -> None:
    """Parse an in-memory JSON/NDJSON buffer and write a new single-sheet workbook.

    Same semantics as :func:`json_to_sheet`; the document never touches disk.
    """
    if isinstance(xlsx_out, Path):
        xlsx_out = expanduser(str(xlsx_out))
    _kyrax.json_bytes_to_sheet(json_bytes, str(xlsx_out), sheet_name, shape, has_header)


__all__ = (
    "sheet_to_csv",
    "sheet_to_csv_bytes",
    "csv_to_sheet",
    "csv_bytes_to_sheet",
    "sheet_to_json",
    "sheet_to_json_bytes",
    "json_to_sheet",
    "json_bytes_to_sheet",
)
