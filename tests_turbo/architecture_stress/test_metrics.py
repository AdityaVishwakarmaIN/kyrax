"""A6 quick harness checks plus opt-in COM, GIL, and scale gates."""

from __future__ import annotations

import ctypes
import json
import os
import random
import statistics
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Callable

import pytest

try:
    from . import common, fixtures
except ImportError:
    import common  # type: ignore[no-redef]
    import fixtures  # type: ignore[no-redef]


OPT_IN_COM = os.environ.get("KYRAX_ARCHSTRESS_EXCEL_COM") == "1"
OPT_IN_LARGE = os.environ.get("KYRAX_ARCHSTRESS_LARGE") == "1"
OPT_IN_NORTHSTAR = os.environ.get("KYRAX_ARCHSTRESS_NORTHSTAR") == "1"


def _process_alive(pid: int) -> bool:
    if sys.platform != "win32":
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
    kernel32.OpenProcess.restype = ctypes.c_void_p
    kernel32.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
    handle = kernel32.OpenProcess(0x00100000 | 0x1000, False, pid)
    if not handle:
        return False
    try:
        return kernel32.WaitForSingleObject(handle, 0) == 0x102
    finally:
        kernel32.CloseHandle(handle)


def test_watchdog_pass_and_nonzero_fail() -> None:
    root = tempfile.gettempdir()
    passed = common.watchdog_run(
        [sys.executable, "-c", "print('ok')"],
        timeout_s=5,
        workdir=root,
        label="pytest_pass",
    )
    failed = common.watchdog_run(
        [sys.executable, "-c", "raise SystemExit(7)"],
        timeout_s=5,
        workdir=root,
        label="pytest_fail",
    )
    assert (passed.verdict, passed.exit_code) == ("PASS", 0)
    assert (failed.verdict, failed.exit_code) == ("FAIL", 7)
    assert passed.isolation in {"windows-job-object", "windows-taskkill-fallback", "posix-session"}


def test_watchdog_timeout_kills_descendant() -> None:
    worker = (
        "import subprocess,sys,time; "
        "p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); "
        "print(p.pid,flush=True); time.sleep(60)"
    )
    result = common.watchdog_run(
        [sys.executable, "-c", worker],
        timeout_s=1.0,
        workdir=tempfile.gettempdir(),
        label="pytest_timeout",
    )
    child_pid = int(Path(result.stdout_path).read_text(encoding="utf-8").splitlines()[0])
    assert result.verdict == "TIMEOUT"
    assert result.cleanup_verified is True, result
    assert result.cleanup_live_pids == []
    assert not _process_alive(child_pid)


def test_watchdog_repeated_timeout_cleans_child_and_grandchild(tmp_path: Path) -> None:
    grandchild = (
        "import os,sys,time; from pathlib import Path; "
        "Path(sys.argv[1]).write_text(str(os.getpid()),encoding='ascii'); "
        "time.sleep(60)"
    )
    child = (
        "import os,subprocess,sys,time\n"
        "from pathlib import Path\n"
        "print('CHILD',os.getpid(),flush=True)\n"
        f"subprocess.Popen([sys.executable,'-c',{grandchild!r},sys.argv[1]])\n"
        "deadline=time.monotonic()+5\n"
        "ready=Path(sys.argv[1])\n"
        "while not ready.is_file() and time.monotonic()<deadline:\n"
        " time.sleep(0.01)\n"
        "assert ready.is_file()\n"
        "print('TREE_READY',flush=True)\n"
        "time.sleep(60)\n"
    )
    for iteration in range(3):
        ready_path = tmp_path / f"tree_{iteration}.ready"
        worker = (
            "import subprocess,sys,time; "
            f"subprocess.Popen([sys.executable,'-c',{child!r},sys.argv[1]]); "
            "time.sleep(60)"
        )
        result = common.watchdog_run(
            [sys.executable, "-c", worker, str(ready_path)],
            timeout_s=2.0,
            workdir=tempfile.gettempdir(),
            label=f"pytest_timeout_tree_{iteration}",
        )
        lines = Path(result.stdout_path).read_text(encoding="utf-8").splitlines()
        child_pids = [int(line.split()[1]) for line in lines if line.startswith("CHILD ")]
        assert lines[-1:] == ["TREE_READY"], (lines, result)
        assert len(child_pids) == 1, (lines, result)
        assert ready_path.is_file(), (lines, result)
        grandchild_pid = int(ready_path.read_text(encoding="ascii"))
        pids = child_pids + [grandchild_pid]
        assert result.verdict == "TIMEOUT"
        assert result.cleanup_verified is True, result
        assert result.cleanup_live_pids == []
        assert all(not _process_alive(pid) for pid in pids), (pids, result)


def test_watchdog_bounded_memory_kill() -> None:
    worker = (
        "import time; chunks=[]\n"
        "while True:\n"
        " chunks.append(bytearray(8*1024*1024)); time.sleep(0.03)"
    )
    result = common.watchdog_run(
        [sys.executable, "-c", worker],
        timeout_s=10,
        rss_limit_mb=96,
        workdir=tempfile.gettempdir(),
        label="pytest_memory",
    )
    assert result.verdict in {"RSS-KILL", "COMMIT-KILL"}
    assert result.peak_ws_mb is not None
    assert "process" in result.rss_sampling or "snapshot" in result.rss_sampling


def test_aggregator_schema_uniqueness_and_atomic_order(tmp_path: Path) -> None:
    aggregator = common.ResultAggregator(run_id="run-fixed")
    later = common.ResultRecord(
        run_id="run-fixed", test_id="B", verdict=common.Verdict.PASS,
        isolation="unit", wave=1, lane="A6",
    )
    earlier = common.ResultRecord(
        run_id="run-fixed", test_id="A", verdict=common.Verdict.KNOWN_GAP,
        isolation="unit", wave=1, lane="A6",
    )
    assert aggregator.add_result(later) == []
    assert aggregator.add_result(earlier) == []
    assert aggregator.add_result(earlier)[0].startswith("duplicate")
    output = tmp_path / "results.jsonl"
    summary = aggregator.publish(output)
    loaded = common.load_jsonl(output)
    assert summary["records"] == 2
    assert [entry["test_id"] for entry in loaded[1:-1]] == ["A", "B"]
    assert not output.with_name(output.name + ".tmp").exists()


def test_environment_snapshot_and_fixture_determinism(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(
        common,
        "excel_com_version_probe",
        lambda timeout_s=15.0: {"available": True, "version": "16.0", "verdict": "PASS"},
    )
    snapshot = common.environment_snapshot(common.EVIDENCE_PYD_SHA256, Path.cwd())
    assert snapshot["git_commit"]
    assert snapshot["pyd_sha256_expected"] == common.EVIDENCE_PYD_SHA256
    fixture_summary = fixtures.self_test(tmp_path)
    assert fixture_summary["fixture_count"] == 1
    assert fixture_summary["artifact_count"] == 3
    assert fixture_summary["canonical_blocked"] == 11
    assert len(fixture_summary["sha256"]) == 64


def _counter_rate(arm: str, duration_s: float, operation: Callable[[], None]) -> float:
    gil_sleeper = None
    gil_sleep_arg = 0
    if arm == "gil_hold":
        if sys.platform == "win32":
            gil_sleeper = ctypes.PyDLL("kernel32", use_last_error=True).Sleep
            gil_sleeper.argtypes = [ctypes.c_ulong]
            gil_sleeper.restype = None
            gil_sleep_arg = max(1, int(duration_s * 1000))
        else:
            gil_sleeper = ctypes.PyDLL(None).usleep
            gil_sleeper.argtypes = [ctypes.c_uint]
            gil_sleeper.restype = ctypes.c_int
            gil_sleep_arg = max(1, int(duration_s * 1_000_000))
    ready = threading.Event()
    stop = threading.Event()
    counter = [0]

    def count() -> None:
        ready.set()
        while not stop.is_set():
            counter[0] += 1

    thread = threading.Thread(target=count, daemon=True)
    thread.start()
    assert ready.wait(2)
    before = counter[0]
    started = time.perf_counter()
    try:
        if arm == "sleep":
            time.sleep(duration_s)
        elif arm == "gil_hold":
            # PyDLL deliberately retains the GIL for the foreign call, unlike
            # CDLL/WinDLL. It is a stable control without interpreter-global
            # switch-interval manipulation.
            assert gil_sleeper is not None
            gil_sleeper(gil_sleep_arg)
        elif arm == "op":
            operation()
        else:
            raise ValueError(f"unknown GIL arm: {arm}")
    finally:
        ended = time.perf_counter()
        measured_count = counter[0]
        stop.set()
        thread.join(timeout=2)
        elapsed = max(ended - started, 1e-9)
    return (measured_count - before) / elapsed


def run_gil_protocol(
    operation_factory: Callable[[float], Callable[[], None]],
    *,
    duration_s: float = 2.0,
    bootstrap_samples: int = 10_000,
) -> dict:
    """Run the preregistered three-triad, counterbalanced GIL protocol."""

    if duration_s <= 0:
        raise ValueError("duration_s must be positive")
    orders = (
        ("op", "sleep", "gil_hold"),
        ("sleep", "gil_hold", "op"),
        ("gil_hold", "op", "sleep"),
    )
    triads: list[dict[str, float]] = []
    for order in orders:
        rates: dict[str, float] = {}
        for arm in order:
            rates[arm] = _counter_rate(arm, duration_s, operation_factory(duration_s))
        triads.append(rates)

    sleep_rates = [row["sleep"] for row in triads]
    hold_rates = [row["gil_hold"] for row in triads]
    op_rates = [row["op"] for row in triads]
    ratios = [row["op"] / max(row["sleep"], 1e-12) for row in triads]
    rng = random.Random(42)
    boot = [
        statistics.median(rng.choice(ratios) for _ in range(3))
        for _ in range(bootstrap_samples)
    ]
    boot.sort()
    ci = (boot[int(0.025 * (len(boot) - 1))], boot[int(0.975 * (len(boot) - 1))])
    sleep_median = statistics.median(sleep_rates)
    hold_max = max(hold_rates)
    calibrated = sleep_median > 0 and sleep_median / max(hold_max, 1e-12) >= 20
    if not calibrated:
        verdict = "BLOCKED"
    elif (
        min(op_rates) / sleep_median >= 0.75
        and min(op_rates) > hold_max * 5
        and ci[0] >= 0.5
    ):
        verdict = "PASS"
    elif statistics.median(op_rates) / sleep_median <= 0.25 or statistics.median(op_rates) <= hold_max * 1.1:
        verdict = "FAIL"
    else:
        verdict = "BLOCKED-ORACLE"
    return {
        "verdict": verdict,
        "duration_s": duration_s,
        "triads": triads,
        "paired_ratios": ratios,
        "bootstrap_95_ci_coarse_n3": ci,
        "calibrated": calibrated,
    }


def test_gil_protocol_quick_mechanics() -> None:
    result = run_gil_protocol(
        lambda duration: lambda: time.sleep(duration),
        duration_s=0.5,
        bootstrap_samples=500,
    )
    assert result["calibrated"]
    # The quick run validates controls and record shape. Only the opt-in
    # two-second protocol is allowed to close a campaign PASS verdict.
    assert result["verdict"] in {"PASS", "BLOCKED-ORACLE"}
    assert len(result["triads"]) == 3


@pytest.mark.excel_com
@pytest.mark.skipif(not OPT_IN_COM, reason="set KYRAX_ARCHSTRESS_EXCEL_COM=1")
def test_excel_com_version_opt_in() -> None:
    result = common.excel_com_version_probe(timeout_s=20)
    assert result["available"] and result["version"] == "16.0", result


@pytest.mark.large
@pytest.mark.skipif(not OPT_IN_LARGE, reason="set KYRAX_ARCHSTRESS_LARGE=1")
def test_large_specs_are_preflighted_not_generated(tmp_path: Path) -> None:
    registry = fixtures.FixtureRegistry()
    for spec in fixtures.scale_specs():
        registry.register(spec)
        ok, detail = registry.preflight(spec.fixture_id, tmp_path)
        assert isinstance(ok, bool) and detail
    # This is deliberately metadata/preflight only. Large generators do not yet exist.
    assert all(not record["present"] for record in registry.records())


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_sleep_control_full_protocol_opt_in() -> None:
    result = run_gil_protocol(lambda duration: lambda: time.sleep(duration), duration_s=2.0)
    assert result["verdict"] == "PASS", result


F01 = Path(__file__).resolve().parents[2] / "testdata" / "structured.xlsx"


def _repeating_operation_factory(
    operation: Callable[[], object], iteration_counts: list[int]
) -> Callable[[float], Callable[[], None]]:
    """Repeat a short public operation until its timed arm lasts >= duration."""

    def factory(duration_s: float) -> Callable[[], None]:
        def run() -> None:
            deadline = time.perf_counter() + duration_s
            count = 0
            while count == 0 or time.perf_counter() < deadline:
                operation()
                count += 1
            iteration_counts.append(count)

        return run

    return factory


def _emit_operation_gil_result(test_id: str, result: dict) -> None:
    result["test_id"] = test_id
    result["fixture"] = {
        "id": "F01",
        "path": str(F01),
        "sha256": common.sha256_file(F01),
    }
    encoded = json.dumps(result, indent=2, sort_keys=True)
    print(f"\nA6_GIL_RESULT {test_id}\n{encoded}")
    output_root = os.environ.get("KYRAX_ARCHSTRESS_OUTPUT")
    if output_root:
        common.atomic_write_json(Path(output_root) / f"{test_id}.json", result)


def _run_operation_gil(test_id: str, operation: Callable[[], object]) -> dict:
    counts: list[int] = []
    result = run_gil_protocol(
        _repeating_operation_factory(operation, counts),
        duration_s=2.0,
        bootstrap_samples=10_000,
    )
    result["operation_iterations"] = counts
    _emit_operation_gil_result(test_id, result)
    assert result["verdict"] in {"PASS", "FAIL", "BLOCKED-ORACLE"}, result
    return result


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_stock_read_opt_in() -> None:
    import kyrax

    assert F01.is_file()
    _run_operation_gil("A6-GIL-STOCK-READ", lambda: kyrax.read_excel(F01))


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_stock_materialize_opt_in() -> None:
    import kyrax

    reader = kyrax.read_excel(F01)  # Setup is outside every timed closure.
    _run_operation_gil(
        "A6-GIL-STOCK-MATERIALIZE",
        lambda: reader.load_sheet("Sheet1", eager=True),
    )


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_turbo_read_opt_in() -> None:
    import kyrax

    _run_operation_gil("A6-GIL-TURBO-READ", lambda: kyrax.read_excel_turbo(F01))


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_turbo_features_all_materialize_opt_in() -> None:
    import kyrax

    reader = kyrax.read_excel_turbo(F01)  # Setup is outside every timed closure.
    _run_operation_gil(
        "A6-GIL-TURBO-ALL-MATERIALIZE",
        lambda: reader.load_sheet("Sheet1", features="all"),
    )


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_small_write_opt_in(tmp_path: Path) -> None:
    import kyrax

    output = tmp_path / "gil-small-write.xlsx"
    sheets = [{
        "name": "Data",
        "rows": [["id", "value"]] + [[index, index * 2] for index in range(2_000)],
    }]
    _run_operation_gil(
        "A6-GIL-SMALL-WRITE",
        lambda: kyrax.write_excel_turbo(output, sheets),
    )
    assert output.is_file()


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_validate_opt_in() -> None:
    import kyrax

    _run_operation_gil("A6-GIL-VALIDATE", lambda: kyrax.validate_excel(F01))


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_repair_opt_in(tmp_path: Path) -> None:
    import kyrax

    output = tmp_path / "gil-repaired.xlsx"
    _run_operation_gil(
        "A6-GIL-REPAIR",
        lambda: kyrax.repair_excel(F01, output, severity="warning"),
    )
    assert output.is_file()


@pytest.mark.northstar
@pytest.mark.skipif(not OPT_IN_NORTHSTAR, reason="set KYRAX_ARCHSTRESS_NORTHSTAR=1")
def test_gil_encrypted_read_blocked_f12() -> None:
    result = {
        "test_id": "A6-GIL-ENCRYPTED-READ",
        "verdict": "BLOCKED",
        "fixture": {"id": "F12", "present": False},
        "detail": (
            "Pinned F12 password-bearing decrypt corpus is unavailable. "
            "is_encrypted() is only a header probe and is not accepted as evidence "
            "that decrypt work releases the GIL."
        ),
    }
    print("\nA6_GIL_RESULT A6-GIL-ENCRYPTED-READ\n" + json.dumps(result, indent=2, sort_keys=True))
    output_root = os.environ.get("KYRAX_ARCHSTRESS_OUTPUT")
    if output_root:
        common.atomic_write_json(Path(output_root) / "A6-GIL-ENCRYPTED-READ.json", result)
    pytest.skip(result["detail"])
