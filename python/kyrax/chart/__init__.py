"""openpyxl-compatible chart data holders for ``ws.add_chart(chart, "F2")``.

These classes only record what the user set; the Rust engine turns them into
series references and writes the chart parts. Supported: bar/column, line,
pie, scatter and area charts with a title, style, size, grouping and legend
position. Axis titles are accepted but not rendered yet.
"""

from __future__ import annotations

from typing import Any


class Reference:
    """A cell block on a worksheet (1-based, openpyxl argument order)."""

    def __init__(
        self,
        worksheet: Any = None,
        min_col: int | None = None,
        min_row: int | None = None,
        max_col: int | None = None,
        max_row: int | None = None,
        range_string: str | None = None,
    ) -> None:
        self.worksheet = worksheet
        self.min_col = min_col
        self.min_row = min_row
        self.max_col = max_col
        self.max_row = max_row
        self.range_string = range_string


class Series:
    """One data series: ``values`` (and ``xvalues`` for scatter / categories)."""

    def __init__(
        self,
        values: Reference,
        xvalues: Reference | None = None,
        zvalues: Reference | None = None,
        title: str | None = None,
        title_from_data: bool = False,
    ) -> None:
        if zvalues is not None:
            raise NotImplementedError("Series zvalues (bubble sizes) are not supported")
        self.values = values
        self.xvalues = xvalues
        self.title = title
        self.title_from_data = title_from_data


class _Axis:
    def __init__(self) -> None:
        self.title: str | None = None  # accepted; not rendered yet


class _Legend:
    def __init__(self) -> None:
        self.position = "r"


class _Chart:
    _kind = ""
    grouping: str | None = None

    def __init__(self) -> None:
        self.title: str | None = None
        self.style: int | None = None
        self.width = 15.0
        self.height = 7.5
        self.anchor = "E15"
        self.legend: _Legend | None = _Legend()
        self.x_axis = _Axis()
        self.y_axis = _Axis()
        self.series: list[Series] = []
        self._data: list[tuple[Reference, bool, bool]] = []
        self._categories: Reference | None = None

    def add_data(
        self, data: Reference, from_rows: bool = False, titles_from_data: bool = False
    ) -> None:
        """Record a block of data; each column (or row) becomes a series on save."""
        self._data.append((data, titles_from_data, from_rows))

    def set_categories(self, labels: Reference) -> None:
        """Record the category (x-axis) labels for the series."""
        self._categories = labels


class BarChart(_Chart):
    _kind = "bar"

    def __init__(self) -> None:
        super().__init__()
        self.type = "col"
        self.grouping = "clustered"


class LineChart(_Chart):
    _kind = "line"

    def __init__(self) -> None:
        super().__init__()
        self.grouping = "standard"


class AreaChart(_Chart):
    _kind = "area"

    def __init__(self) -> None:
        super().__init__()
        self.grouping = "standard"


class PieChart(_Chart):
    _kind = "pie"


class ScatterChart(_Chart):
    _kind = "scatter"


__all__ = [
    "AreaChart",
    "BarChart",
    "LineChart",
    "PieChart",
    "Reference",
    "ScatterChart",
    "Series",
]
