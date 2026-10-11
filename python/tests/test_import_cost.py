"""`import kyrax` must stay cheap, and the pyarrow check must be lazy AND correct."""

from __future__ import annotations

import io
import subprocess
import sys
import textwrap
from pathlib import Path

import kyrax
import pytest

_HEAVY = ("importlib.util", "importlib.resources", "inspect", "pathlib")


def _run(code: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(code)],
        capture_output=True,
        text=True,
        check=False,
    )


def test_import_does_not_pull_heavy_modules():
    proc = _run(
        f"""
        import sys
        before = set(sys.modules)
        import kyrax
        heavy = [m for m in {_HEAVY!r} if m in sys.modules and m not in before]
        print(heavy)
        """
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip() == "[]", proc.stdout


def test_pyarrow_available_in_normal_env():
    pytest.importorskip("pyarrow")
    proc = _run(
        """
        import sys
        had_util = "importlib.util" in sys.modules
        import kyrax
        # `import kyrax` alone must not have loaded importlib.util ...
        assert had_util or "importlib.util" not in sys.modules
        # ... the lazy check does, on first use
        assert kyrax._PYARROW_AVAILABLE is True
        assert kyrax._pyarrow_available() is True
        print("ok")
        """
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert proc.stdout.strip() == "ok"


def test_pyarrow_blocked_is_reported_missing():
    proc = _run(
        """
        import sys
        sys.modules["pyarrow"] = None
        import kyrax
        assert kyrax._PYARROW_AVAILABLE is False
        assert kyrax._pyarrow_available() is False
        sheet = kyrax.read_excel(
            kyrax.write_excel_turbo_bytes([{"name": "S", "rows": [["a"], [1]]}])
        ).load_sheet(0)
        try:
            sheet.to_arrow()
        except ImportError as e:
            assert "pyarrow is required for to_arrow()" in str(e), e
        else:
            raise SystemExit("to_arrow() did not raise ImportError")
        try:
            sheet.to_arrow_with_errors()
        except ImportError as e:
            assert "pyarrow is required" in str(e), e
        else:
            raise SystemExit("to_arrow_with_errors() did not raise ImportError")
        print("ok")
        """
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert proc.stdout.strip() == "ok"


def test_pyarrow_found_only_via_meta_path_finder():
    proc = _run(
        """
        import sys
        from importlib.machinery import ModuleSpec

        class StubFinder:
            def find_spec(self, name, path=None, target=None):
                return ModuleSpec("pyarrow", None) if name == "pyarrow" else None

        import kyrax
        import importlib.util  # must be importable before PathFinder is removed
        sys.modules.pop("pyarrow", None)
        # the stub is the ONLY way to find pyarrow now (no PathFinder at all)
        sys.meta_path[:] = [StubFinder()]
        assert kyrax._pyarrow_available() is True
        print("ok")
        """
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert proc.stdout.strip() == "ok"


def test_pyarrow_check_is_cached_and_wraps_find_spec_errors(monkeypatch):
    import importlib.util

    monkeypatch.setattr(kyrax, "_pyarrow_available_cache", None)
    monkeypatch.delitem(sys.modules, "pyarrow", raising=False)

    def boom(name):
        raise ValueError("pyarrow.__spec__ is None")

    monkeypatch.setattr(importlib.util, "find_spec", boom)
    assert kyrax._pyarrow_available() is False
    # cached: no second lookup even though find_spec would now succeed
    monkeypatch.setattr(importlib.util, "find_spec", lambda name: object())
    assert kyrax._pyarrow_available() is False


def test_pyarrow_missing_spec_is_false(monkeypatch):
    import importlib.util

    monkeypatch.setattr(kyrax, "_pyarrow_available_cache", None)
    monkeypatch.delitem(sys.modules, "pyarrow", raising=False)
    monkeypatch.setattr(importlib.util, "find_spec", lambda name: None)
    assert kyrax._pyarrow_available() is False


class _PathLike:
    def __init__(self, p):
        self._p = p

    def __fspath__(self):
        return str(self._p)


@pytest.fixture
def home(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))
    return tmp_path


def _variants(path: Path, home: Path):
    tilde = "~/" + str(path.relative_to(home))
    return [path, str(path), _PathLike(path), tilde, Path(tilde)]


def _sheets():
    return [{"name": "S", "rows": [["a", "b"], [1, 2]]}]


def test_path_variants_write(home):
    target = home / "w.xlsx"
    for spec in _variants(target, home):
        if target.exists():
            target.unlink()
        kyrax.write_excel_turbo(spec, _sheets())
        assert target.exists(), spec
    for spec in _variants(home / "s.xlsx", home):
        kyrax.write_excel_turbo_stream(spec, _sheets())
        assert (home / "s.xlsx").exists(), spec
        (home / "s.xlsx").unlink()


def test_path_variants_read(home):
    src = home / "r.xlsx"
    kyrax.write_excel_turbo(src, _sheets())
    for spec in _variants(src, home):
        assert kyrax.read_excel(spec).sheet_names == ["S"], spec
        assert kyrax.read_excel_turbo(spec).sheet_names == ["S"], spec
        assert kyrax.load_workbook(spec).sheetnames == ["S"], spec
        assert kyrax.load_workbook(spec, read_only=True).sheetnames == ["S"], spec


def test_bytes_and_filelike_inputs_unchanged(home):
    data = kyrax.write_excel_turbo_bytes(_sheets())
    assert kyrax.read_excel(data).sheet_names == ["S"]
    assert kyrax.load_workbook(io.BytesIO(data)).sheetnames == ["S"]
    assert kyrax.load_workbook(data).sheetnames == ["S"]


def test_validate_repair_is_encrypted_accept_paths(home):
    src = home / "v.xlsx"
    kyrax.write_excel_turbo(src, _sheets())
    for spec in (src, str(src), _PathLike(src), Path("~/v.xlsx")):
        # str is deliberately not tilde-expanded here (unchanged behaviour); only
        # absolute spellings are asserted for str.
        assert kyrax.validate_excel(spec)["valid"] is True, spec
        assert kyrax.is_encrypted(spec) is False, spec
    kyrax.repair_excel(src, home / "fixed.xlsx")
    kyrax.repair_excel(Path("~/v.xlsx"), Path("~/fixed2.xlsx"))
    kyrax.repair_excel(_PathLike(src), _PathLike(home / "fixed3.xlsx"))


def test_vba_archive_path_is_expanded(home):
    # A missing archive must fail on the *expanded* path, not on a literal "~/...".
    with pytest.raises(Exception) as ei:
        kyrax.write_excel_turbo(
            home / "m.xlsm",
            _sheets(),
            macro_enabled=True,
            vba_archive_path=Path("~/nope.xlsm"),
        )
    assert str(home) in str(ei.value) or "~/nope" not in str(ei.value)
