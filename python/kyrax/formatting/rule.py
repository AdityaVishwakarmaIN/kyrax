"""openpyxl-compatible conditional-formatting rule holders.

Thin data holders only: each object converts to the CF rule dict that the
Rust engine already parses (``_kx_spec``). Use with
``ws.conditional_formatting.add("B2:B10", rule)``.
"""

from __future__ import annotations

from typing import Any

# openpyxl CellIsRule operator shorthands -> OOXML operator names.
_OPERATORS = {
    ">": "greaterThan",
    ">=": "greaterThanOrEqual",
    "<": "lessThan",
    "<=": "lessThanOrEqual",
    "=": "equal",
    "==": "equal",
    "!=": "notEqual",
}


def _formulas(formula: Any) -> list[str]:
    if formula is None:
        return []
    if isinstance(formula, (list, tuple)):
        return [str(f) for f in formula]
    return [str(formula)]


class DifferentialStyle:
    """openpyxl ``DifferentialStyle`` (font / fill / border of a CF rule)."""

    def __init__(self, font: Any = None, fill: Any = None, border: Any = None, **_: Any) -> None:
        self.font = font
        self.fill = fill
        self.border = border

    def _kx_spec(self) -> dict[str, Any]:
        return {k: v for k, v in (("font", self.font), ("fill", self.fill), ("border", self.border)) if v is not None}


class Rule:
    """openpyxl ``Rule``: a generic conditional-formatting rule."""

    def __init__(
        self,
        type: str,
        dxf: Any = None,
        priority: int | None = None,
        stopIfTrue: bool | None = None,
        operator: str | None = None,
        text: str | None = None,
        timePeriod: str | None = None,
        rank: int | None = None,
        percent: bool | None = None,
        bottom: bool | None = None,
        aboveAverage: bool | None = None,
        equalAverage: bool | None = None,
        stdDev: int | None = None,
        formula: Any = None,
        colorScale: dict[str, Any] | None = None,
        dataBar: dict[str, Any] | None = None,
        iconSet: dict[str, Any] | None = None,
        **_: Any,
    ) -> None:
        self.type = type
        self.dxf = dxf
        self.priority = priority
        self.stopIfTrue = stopIfTrue
        self.operator = operator
        self.text = text
        self.timePeriod = timePeriod
        self.rank = rank
        self.percent = percent
        self.bottom = bottom
        self.aboveAverage = aboveAverage
        self.equalAverage = equalAverage
        self.stdDev = stdDev
        self.formula = _formulas(formula)
        self.colorScale = colorScale
        self.dataBar = dataBar
        self.iconSet = iconSet

    def _kx_spec(self) -> dict[str, Any]:
        spec: dict[str, Any] = {"type": self.type}
        for key in ("operator", "text", "timePeriod", "rank", "percent", "bottom",
                    "aboveAverage", "equalAverage", "stdDev", "stopIfTrue"):
            val = getattr(self, key)
            if val is not None:
                spec[key] = val
        if self.formula:
            spec["formulas"] = list(self.formula)
        if self.dxf is not None:
            spec["dxf"] = self.dxf._kx_spec() if hasattr(self.dxf, "_kx_spec") else self.dxf
        for extra in (self.colorScale, self.dataBar, self.iconSet):
            if extra:
                spec.update(extra)
        return spec

    def __repr__(self) -> str:
        return f"<Rule type={self.type!r} formula={self.formula!r}>"


def _dxf(font: Any, fill: Any, border: Any) -> DifferentialStyle | None:
    if font is None and fill is None and border is None:
        return None
    return DifferentialStyle(font=font, fill=fill, border=border)


def _cfvo(type_: str | None, value: Any) -> dict[str, Any]:
    vo: dict[str, Any] = {"type": type_ or "min"}
    if value is not None:
        vo["val"] = str(value)
    return vo


def CellIsRule(
    operator: str | None = None,
    formula: Any = None,
    stopIfTrue: bool | None = None,
    font: Any = None,
    border: Any = None,
    fill: Any = None,
) -> Rule:
    """openpyxl ``CellIsRule``: compare the cell value with ``formula``."""
    op = _OPERATORS.get(operator, operator) if operator else None
    return Rule(type="cellIs", operator=op, formula=formula, stopIfTrue=stopIfTrue,
                dxf=_dxf(font, fill, border))


def FormulaRule(
    formula: Any = None,
    stopIfTrue: bool | None = None,
    font: Any = None,
    border: Any = None,
    fill: Any = None,
) -> Rule:
    """openpyxl ``FormulaRule``: format where ``formula`` is true."""
    return Rule(type="expression", formula=formula, stopIfTrue=stopIfTrue,
                dxf=_dxf(font, fill, border))


def ColorScaleRule(
    start_type: str | None = None,
    start_value: Any = None,
    start_color: Any = None,
    mid_type: str | None = None,
    mid_value: Any = None,
    mid_color: Any = None,
    end_type: str | None = None,
    end_value: Any = None,
    end_color: Any = None,
) -> Rule:
    """openpyxl ``ColorScaleRule`` (2- or 3-color scale)."""
    cfvos = [_cfvo(start_type, start_value)]
    colors = [start_color]
    if mid_type is not None:
        cfvos.append(_cfvo(mid_type, mid_value))
        colors.append(mid_color)
    cfvos.append(_cfvo(end_type, end_value))
    colors.append(end_color)
    return Rule(type="colorScale", colorScale={"cfvos": cfvos, "colors": colors})


def DataBarRule(
    start_type: str | None = None,
    start_value: Any = None,
    end_type: str | None = None,
    end_value: Any = None,
    color: Any = None,
    showValue: bool | None = None,
    minLength: int | None = None,
    maxLength: int | None = None,
) -> Rule:
    """openpyxl ``DataBarRule``."""
    bar: dict[str, Any] = {"cfvos": [_cfvo(start_type, start_value), _cfvo(end_type, end_value)]}
    for key, val in (("color", color), ("showValue", showValue),
                     ("minLength", minLength), ("maxLength", maxLength)):
        if val is not None:
            bar[key] = val
    return Rule(type="dataBar", dataBar=bar)


def IconSetRule(
    icon_style: str | None = None,
    type: str | None = None,
    values: Any = None,
    showValue: bool | None = None,
    percent: bool | None = None,
    reverse: bool | None = None,
) -> Rule:
    """openpyxl ``IconSetRule``."""
    icons: dict[str, Any] = {
        "iconSet": icon_style or "3TrafficLights1",
        "cfvos": [_cfvo(type, v) for v in (values or [])],
    }
    for key, val in (("showValue", showValue), ("percent", percent), ("reverse", reverse)):
        if val is not None:
            icons[key] = val
    return Rule(type="iconSet", iconSet=icons)


__all__ = [
    "CellIsRule",
    "ColorScaleRule",
    "DataBarRule",
    "DifferentialStyle",
    "FormulaRule",
    "IconSetRule",
    "Rule",
]
