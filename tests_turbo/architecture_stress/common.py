"""Shared evidence harness for the architecture stress campaign (A6).

Single-writer JSONL evidence protocol, exact-tree watchdog with safe platform
behavior, sampled working-set RSS, SHA-256 manifests, preflight, environment
fingerprints, and a bounded Excel COM version probe. Stdlib only.

Containment status: POSIX uses a new session plus killpg tree cleanup. Windows
assigns the exact worker to a kill-on-close Job Object. If assignment is denied
(for example by a restrictive outer job), the result is explicitly labeled as
the `taskkill /PID <pid> /T /F` fallback. Commit limits and sampled resident
working-set measurements are deliberately reported as different quantities.
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import zipfile
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Any, Iterable, Mapping, MutableMapping, Optional, Sequence

IS_WINDOWS = sys.platform == "win32"
IS_POSIX = not IS_WINDOWS

SCHEMA_VERSION = 1

EVIDENCE_PYD_SHA256 = "A7076646894AED415061BBA748BD363490BE6275CDA1DBA16FB3F115E3973BE5"
EVIDENCE_STRUCTURED_S = 3.288
EVIDENCE_EXCEL_VERSION = "16.0"
EVIDENCE_DISK_FREE_GIB = 391.34
EVIDENCE_RAM_GIB = 15.69

JOBOBJECTINFOCLASS_EXTENDED_LIMIT = 9
JOBOBJECTINFOCLASS_PROCESS_ID_LIST = 3
JOBOBJECTINFOCLASS_LIMIT_VIOLATION = 13
JOBOBJECTINFOCLASS_ASSOCIATE_COMPLETION_PORT = 7
JOB_OBJECT_LIMIT_PROCESS_MEMORY = 0x100
JOB_OBJECT_LIMIT_JOB_MEMORY = 0x200
JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000
JOB_OBJECT_IMPLEMENTED = IS_WINDOWS
JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT = 9
JOB_OBJECT_MSG_JOB_MEMORY_LIMIT = 10

WATCHDOG_TICK_S = 0.1
POSIX_TERM_GRACE_S = 2.0
TASKKILL_TIMEOUT_S = 10.0
WINDOWS_CLEANUP_GRACE_S = 5.0

ORDER_KEYS = ("run_id", "wave", "lane", "test_id")


class Verdict(str, Enum):
    PASS = "PASS"
    FAIL = "FAIL"
    KNOWN_GAP = "KNOWN-GAP"
    BLOCKED = "BLOCKED"
    BLOCKED_ORACLE = "BLOCKED-ORACLE"
    BLOCKED_RESOURCE = "BLOCKED-RESOURCE"
    N_A = "N/A"
    TIMEOUT = "TIMEOUT"
    RSS_KILL = "RSS-KILL"
    COMMIT_KILL = "COMMIT-KILL"
    CRASH = "CRASH"
    COM_UNAVAILABLE = "COM-UNAVAILABLE"


REQUIRED_KEYS = (
    "run_id",
    "test_id",
    "verdict",
    "ts",
    "platform",
    "isolation",
    "schema_version",
)

TYPE_RULES: Mapping[str, tuple] = {
    "run_id": (str,),
    "test_id": (str,),
    "verdict": (str,),
    "ts": (str,),
    "platform": (dict,),
    "isolation": (str,),
    "schema_version": (int,),
    "duration_s": (int, float, type(None)),
    "exit_code": (int, type(None)),
    "peak_ws_mb": (int, float, type(None)),
    "peak_commit_mb": (int, float, type(None)),
    "cpu_sec": (int, float, type(None)),
    "detail": (str, type(None)),
}


def utcnow_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def new_run_id() -> str:
    return f"{os.getpid()}-{int(time.time() * 1000)}-{hashlib.sha256(os.urandom(8)).hexdigest()[:8]}"


@dataclass
class ResultRecord:
    run_id: str
    test_id: str
    verdict: str
    isolation: str = "unknown"
    duration_s: Optional[float] = None
    exit_code: Optional[int] = None
    peak_ws_mb: Optional[float] = None
    peak_commit_mb: Optional[float] = None
    cpu_sec: Optional[float] = None
    detail: Optional[str] = None
    wave: Optional[int] = None
    lane: Optional[str] = None
    command: Optional[str] = None
    platform: dict = field(default_factory=lambda: {
        "os": platform.system(), "python": platform.python_version()
    })
    rss_sampling: Optional[str] = None
    fingerprint: Optional[dict] = None
    fixture: Optional[dict] = None
    measured: Optional[dict] = field(default_factory=dict)
    extra: Optional[dict] = field(default_factory=dict)

    def to_dict(self) -> dict:
        d = asdict(self)
        d["schema_version"] = SCHEMA_VERSION
        d["ts"] = utcnow_iso()
        d["verdict"] = self.verdict.value if isinstance(self.verdict, Verdict) else str(self.verdict)
        return {k: v for k, v in d.items() if v is not None}

    def to_record(self) -> dict:
        return self.to_dict()


def validate_record(record: Mapping[str, Any]) -> list[str]:
    errors: list[str] = []
    for key in REQUIRED_KEYS:
        if key not in record:
            errors.append(f"missing required key: {key}")
    v = record.get("verdict")
    normalized_verdict = v.value if isinstance(v, Verdict) else str(v)
    if v is not None and normalized_verdict not in {x.value for x in Verdict}:
        errors.append(f"invalid verdict: {v!r}")
    for key, types in TYPE_RULES.items():
        if key in record and not isinstance(record[key], types):
            errors.append(f"{key}: expected {[t.__name__ for t in types]}, got {type(record[key]).__name__}")
    return errors


def sha256_file(path: str | os.PathLike) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def zip_member_manifest(path: str | os.PathLike) -> dict:
    members = []
    with zipfile.ZipFile(path, "r") as zf:
        for info in zf.infolist():
            data = zf.read(info)
            members.append(
                {
                    "name": info.filename,
                    "size": info.file_size,
                    "compressed": info.compress_size,
                    "crc32": f"{info.CRC & 0xFFFFFFFF:08x}",
                    "sha256": sha256_bytes(data),
                }
            )
    members.sort(key=lambda m: m["name"])
    return {"members": members, "container_sha256": sha256_file(path)}


def free_disk_gib(path: str | os.PathLike) -> float:
    return shutil.disk_usage(os.fspath(path)).free / (1024**3)


def total_ram_gib() -> float:
    if IS_WINDOWS:
        try:
            import ctypes
            from ctypes import wintypes

            class MEMORYSTATUSEX(ctypes.Structure):
                _fields_ = [
                    ("dwLength", wintypes.DWORD),
                    ("dwMemoryLoad", wintypes.DWORD),
                    ("ullTotalPhys", ctypes.c_uint64),
                    ("ullAvailPhys", ctypes.c_uint64),
                    ("ullTotalPageFile", ctypes.c_uint64),
                    ("ullAvailPageFile", ctypes.c_uint64),
                    ("ullTotalVirtual", ctypes.c_uint64),
                    ("ullAvailVirtual", ctypes.c_uint64),
                    ("ullAvailExtendedVirtual", ctypes.c_uint64),
                ]

            stat = MEMORYSTATUSEX()
            stat.dwLength = ctypes.sizeof(MEMORYSTATUSEX)
            ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(stat))
            return stat.ullTotalPhys / (1024**3)
        except Exception:
            return 0.0
    try:
        pages = os.sysconf("SC_PHYS_PAGES")
        page = os.sysconf("SC_PAGE_SIZE")
        return pages * page / (1024**3)
    except Exception:
        return 0.0


def resource_preflight(
    path: str | os.PathLike,
    need_input_bytes: int,
    need_output_bytes: int = 0,
    copies: int = 2,
    headroom: float = 1.2,
) -> tuple[bool, str]:
    need = int((need_input_bytes * copies + need_output_bytes) * headroom)
    if need <= 0:
        return True, "no resource need"
    disk = free_disk_gib(path) * (1024**3)
    ok = disk >= need
    detail = f"need {need / 1e9:.2f} GiB, free {disk / 1e9:.2f} GiB"
    return ok, detail


def git_commit(workdir: str | os.PathLike) -> Optional[str]:
    try:
        out = subprocess.run(
            ["git", "-C", os.fspath(workdir), "rev-parse", "HEAD"],
            capture_output=True, text=True, timeout=10,
        )
        if out.returncode == 0:
            return out.stdout.strip()
    except Exception:
        pass
    return None


def git_status_short(workdir: str | os.PathLike, max_lines: int = 200) -> str:
    try:
        out = subprocess.run(
            ["git", "-C", os.fspath(workdir), "status", "--short"],
            capture_output=True, text=True, timeout=10,
        )
        lines = out.stdout.splitlines()
        return "\n".join(lines[:max_lines])
    except Exception:
        return ""


def excel_com_version_probe(timeout_s: float = 15.0) -> dict:
    if not IS_WINDOWS:
        return {"available": False, "version": None, "method": "none",
                "verdict": "COM-UNAVAILABLE",
                "detail": "Excel COM is Windows-only"}
    script = r"""
$baseline = @(Get-Process EXCEL -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$app = $null
$newPids = @()
try {
    $app = New-Object -ComObject Excel.Application
    $app.Visible = $false
    $after = @(Get-Process EXCEL -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
    $newPids = @($after | Where-Object { $baseline -notcontains $_ })
    $version = $app.Version
    if ($newPids.Count -eq 0) {
        [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($app)
        $app = $null
        Write-Output ("COM-REUSED " + $version)
    } else {
        Write-Output ("OK " + $version + " " + ($newPids -join ','))
        $app.Quit()
        Start-Sleep -Milliseconds 300
        foreach ($p in $newPids) {
            if (Get-Process -Id $p -ErrorAction SilentlyContinue) {
                Stop-Process -Id $p -Force -ErrorAction SilentlyContinue
            }
        }
    }
} catch {
    Write-Output ("ERROR " + $_.Exception.Message)
} finally {
    if ($app -ne $null) {
        [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($app)
    }
    # Cleanup is scoped to Excel PIDs created by this probe.  Quit() can
    # transiently return RPC_E_CALL_REJECTED even after Version succeeded.
    foreach ($p in $newPids) {
        if (Get-Process -Id $p -ErrorAction SilentlyContinue) {
            Stop-Process -Id $p -Force -ErrorAction SilentlyContinue
        }
    }
}
"""
    try:
        out = subprocess.run(
            ["powershell", "-NoProfile", "-NonInteractive", "-Command", script],
            capture_output=True, text=True, timeout=timeout_s,
        )
    except subprocess.TimeoutExpired:
        return {"available": False, "version": None, "method": "powershell-com-exact-pid",
                "verdict": "COM-UNAVAILABLE", "detail": "COM probe timed out"}
    except Exception as exc:
        return {"available": False, "version": None, "method": "powershell-com-exact-pid",
                "verdict": "COM-UNAVAILABLE", "detail": f"{type(exc).__name__}: {exc}"}
    output_lines = [line.strip() for line in out.stdout.splitlines() if line.strip()]
    ok_lines = [line for line in output_lines if line.startswith("OK ")]
    last = output_lines[-1] if output_lines else ""
    if ok_lines:
        parts = ok_lines[-1].split()
        version = parts[1] if len(parts) > 1 else None
        cleanup_errors = [line for line in output_lines if line.startswith("ERROR")]
        return {"available": True, "version": version, "method": "powershell-com-exact-pid",
                "verdict": "PASS",
                "spawned_pids": parts[2].split(",") if len(parts) > 2 else [],
                "detail": "; ".join(cleanup_errors)}
    if last.startswith("COM-REUSED"):
        version = last.split()[1] if len(last.split()) > 1 else None
        return {"available": False, "version": version, "method": "powershell-com-exact-pid",
                "verdict": "BLOCKED-ORACLE", "result": "com-reused",
                "detail": "COM reused an existing Excel PID; references released without Quit"}
    if last.startswith("ERROR"):
        return {"available": False, "version": None, "method": "powershell-com-exact-pid",
                "verdict": "COM-UNAVAILABLE", "detail": last}
    return {"available": False, "version": None, "method": "powershell-com-exact-pid",
            "verdict": "COM-UNAVAILABLE", "detail": f"unexpected probe output: {last!r}"}


def environment_snapshot(
    expected_pyd_sha256: Optional[str] = None,
    workdir: str | os.PathLike | None = None,
) -> dict:
    wd = Path(workdir).resolve() if workdir is not None else Path.cwd().resolve()
    snap = {
        "os": platform.system(),
        "os_release": platform.release(),
        "arch": platform.machine(),
        "python": platform.python_version(),
        "executable": sys.executable,
        "cpu_count": os.cpu_count(),
        "ram_gib": round(total_ram_gib(), 2),
        "disk_free_gib": round(free_disk_gib(wd), 2),
        "ts": utcnow_iso(),
        "excel": excel_com_version_probe(),
    }
    commit = git_commit(wd)
    if commit:
        snap["git_commit"] = commit
        snap["git_status_short"] = git_status_short(wd)
    pyd_candidates = [
        wd / "python" / "kyrax" / "_kyrax.pyd",
        wd / ".venv" / "Lib" / "site-packages" / "kyrax" / "_kyrax.pyd",
        Path.cwd() / "_kyrax.pyd",
    ]
    measured_pyd = None
    for p in pyd_candidates:
        if Path(p).exists():
            measured_pyd = sha256_file(p)
            snap["pyd_path"] = str(p)
            snap["pyd_sha256_measured"] = measured_pyd
            snap["pyd_mtime"] = datetime.fromtimestamp(
                Path(p).stat().st_mtime, tz=timezone.utc
            ).isoformat(timespec="seconds")
            break
    if expected_pyd_sha256:
        snap["pyd_sha256_expected"] = expected_pyd_sha256
        snap["pyd_matches_expected"] = (
            measured_pyd.casefold() == expected_pyd_sha256.casefold()
            if measured_pyd else None
        )
    snap["evidence"] = {
        "pyd_sha256": EVIDENCE_PYD_SHA256,
        "structured_read_seconds": EVIDENCE_STRUCTURED_S,
        "excel_version": EVIDENCE_EXCEL_VERSION,
        "disk_free_gib": EVIDENCE_DISK_FREE_GIB,
        "ram_gib": EVIDENCE_RAM_GIB,
    }
    return snap


def atomic_write_json(path: str | os.PathLike, data: Any) -> None:
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_name(p.name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, p)


def get_working_set_bytes(pid: int) -> Optional[int]:
    """Return one process's resident working set, never its commit charge."""

    if not IS_WINDOWS:
        return _posix_rss_bytes(pid)
    try:
        import ctypes
        from ctypes import wintypes

        class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
            _fields_ = [
                ("cb", wintypes.DWORD),
                ("PageFaultCount", wintypes.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t),
                ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        psapi = ctypes.WinDLL("psapi", use_last_error=True)
        kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel32.OpenProcess.restype = wintypes.HANDLE
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel32.CloseHandle.restype = wintypes.BOOL
        psapi.GetProcessMemoryInfo.argtypes = [
            wintypes.HANDLE, ctypes.POINTER(PROCESS_MEMORY_COUNTERS), wintypes.DWORD
        ]
        psapi.GetProcessMemoryInfo.restype = wintypes.BOOL

        # GetProcessMemoryInfo is documented for QUERY_INFORMATION | VM_READ.
        access = 0x0400 | 0x0010
        handle = kernel32.OpenProcess(access, False, pid)
        if not handle:
            return None
        try:
            counters = PROCESS_MEMORY_COUNTERS()
            counters.cb = ctypes.sizeof(counters)
            if not psapi.GetProcessMemoryInfo(
                handle, ctypes.byref(counters), ctypes.sizeof(counters)
            ):
                return None
            return int(counters.WorkingSetSize)
        finally:
            kernel32.CloseHandle(handle)
    except Exception:
        return None


def _posix_rss_bytes(pid: int) -> Optional[int]:
    try:
        with open(f"/proc/{pid}/status", "r", encoding="utf-8") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) * 1024
    except Exception:
        pass
    return None


def _posix_descendant_pids(root_pid: int) -> list[int]:
    parents: dict[int, int] = {}
    try:
        for entry in Path("/proc").iterdir():
            if not entry.name.isdigit():
                continue
            try:
                fields = (entry / "stat").read_text(encoding="utf-8").split()
                parents[int(entry.name)] = int(fields[3])
            except (OSError, ValueError, IndexError):
                continue
    except OSError:
        return [root_pid]
    selected = {root_pid}
    changed = True
    while changed:
        changed = False
        for pid, parent in parents.items():
            if parent in selected and pid not in selected:
                selected.add(pid)
                changed = True
    return sorted(selected)


def _windows_snapshot_descendants(root_pid: int) -> list[int]:
    """Toolhelp fallback used only when a Job Object PID list is unavailable."""

    if not IS_WINDOWS:
        return [root_pid]
    try:
        import ctypes
        from ctypes import wintypes

        class PROCESSENTRY32W(ctypes.Structure):
            _fields_ = [
                ("dwSize", wintypes.DWORD),
                ("cntUsage", wintypes.DWORD),
                ("th32ProcessID", wintypes.DWORD),
                ("th32DefaultHeapID", ctypes.c_size_t),
                ("th32ModuleID", wintypes.DWORD),
                ("cntThreads", wintypes.DWORD),
                ("th32ParentProcessID", wintypes.DWORD),
                ("pcPriClassBase", wintypes.LONG),
                ("dwFlags", wintypes.DWORD),
                ("szExeFile", wintypes.WCHAR * 260),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
        kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
        kernel32.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
        kernel32.Process32FirstW.restype = wintypes.BOOL
        kernel32.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
        kernel32.Process32NextW.restype = wintypes.BOOL
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        snapshot = kernel32.CreateToolhelp32Snapshot(0x00000002, 0)
        if snapshot in (None, 0, ctypes.c_void_p(-1).value):
            return [root_pid]
        parents: dict[int, int] = {}
        try:
            entry = PROCESSENTRY32W()
            entry.dwSize = ctypes.sizeof(entry)
            ok = kernel32.Process32FirstW(snapshot, ctypes.byref(entry))
            while ok:
                parents[int(entry.th32ProcessID)] = int(entry.th32ParentProcessID)
                ok = kernel32.Process32NextW(snapshot, ctypes.byref(entry))
        finally:
            kernel32.CloseHandle(snapshot)
        selected = {root_pid}
        changed = True
        while changed:
            changed = False
            for pid, parent in parents.items():
                if parent in selected and pid not in selected:
                    selected.add(pid)
                    changed = True
        return sorted(selected)
    except Exception:
        return [root_pid]


@dataclass
class _WindowsJob:
    handle: int
    limit_bytes: Optional[int] = None
    completion_port: int = 0
    memory_violation_flags: int = 0

    def close(self) -> None:
        if not self.handle:
            return
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel32.CloseHandle(self.handle)
        self.handle = 0
        if self.completion_port:
            kernel32.CloseHandle(self.completion_port)
            self.completion_port = 0

    def terminate(self, exit_code: int = 1) -> bool:
        if not self.handle:
            return False
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.TerminateJobObject.argtypes = [wintypes.HANDLE, wintypes.UINT]
        kernel32.TerminateJobObject.restype = wintypes.BOOL
        return bool(kernel32.TerminateJobObject(self.handle, exit_code))

    def process_ids(self) -> list[int]:
        if not self.handle:
            return []
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.QueryInformationJobObject.argtypes = [
            wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
            ctypes.POINTER(wintypes.DWORD),
        ]
        kernel32.QueryInformationJobObject.restype = wintypes.BOOL
        capacity = 64
        while capacity <= 16_384:
            size = 8 + capacity * ctypes.sizeof(ctypes.c_size_t)
            buffer = ctypes.create_string_buffer(size)
            returned = wintypes.DWORD()
            ok = kernel32.QueryInformationJobObject(
                self.handle, JOBOBJECTINFOCLASS_PROCESS_ID_LIST,
                buffer, size, ctypes.byref(returned),
            )
            header = ctypes.cast(buffer, ctypes.POINTER(wintypes.DWORD))
            assigned, count = int(header[0]), int(header[1])
            if ok or assigned <= capacity:
                array_type = ctypes.c_size_t * min(count, capacity)
                values = array_type.from_buffer(buffer, 8)
                return [int(pid) for pid in values if pid]
            capacity = max(capacity * 2, assigned)
        return []

    def poll_messages(self, timeout_ms: int = 0) -> int:
        """Drain Job completion messages and retain exact memory-limit events."""

        if not self.completion_port:
            return self.memory_violation_flags
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.GetQueuedCompletionStatus.argtypes = [
            wintypes.HANDLE,
            ctypes.POINTER(wintypes.DWORD),
            ctypes.POINTER(ctypes.c_size_t),
            ctypes.POINTER(ctypes.c_void_p),
            wintypes.DWORD,
        ]
        kernel32.GetQueuedCompletionStatus.restype = wintypes.BOOL
        wait = timeout_ms
        while True:
            message = wintypes.DWORD()
            key = ctypes.c_size_t()
            overlapped = ctypes.c_void_p()
            ok = kernel32.GetQueuedCompletionStatus(
                self.completion_port,
                ctypes.byref(message), ctypes.byref(key), ctypes.byref(overlapped), wait,
            )
            if not ok:
                break
            if message.value == JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT:
                self.memory_violation_flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY
            elif message.value == JOB_OBJECT_MSG_JOB_MEMORY_LIMIT:
                self.memory_violation_flags |= JOB_OBJECT_LIMIT_JOB_MEMORY
            wait = 0
        return self.memory_violation_flags

    def accounting(self) -> tuple[Optional[int], Optional[int], int]:
        """Return peak process commit, peak job commit, violation flags."""

        if not self.handle:
            return None, None, 0
        import ctypes
        from ctypes import wintypes

        class BASIC_LIMIT(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_longlong),
                ("PerJobUserTimeLimit", ctypes.c_longlong),
                ("LimitFlags", wintypes.DWORD),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class IO_COUNTERS(ctypes.Structure):
            _fields_ = [(name, ctypes.c_ulonglong) for name in (
                "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
                "ReadTransferCount", "WriteTransferCount", "OtherTransferCount",
            )]

        class EXTENDED_LIMIT(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", BASIC_LIMIT),
                ("IoInfo", IO_COUNTERS),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        class LIMIT_VIOLATION(ctypes.Structure):
            _fields_ = [
                ("LimitFlags", wintypes.DWORD),
                ("ViolationLimitFlags", wintypes.DWORD),
                ("IoReadBytes", ctypes.c_ulonglong),
                ("IoReadBytesLimit", ctypes.c_ulonglong),
                ("IoWriteBytes", ctypes.c_ulonglong),
                ("IoWriteBytesLimit", ctypes.c_ulonglong),
                ("PerJobUserTime", ctypes.c_longlong),
                ("PerJobUserTimeLimit", ctypes.c_longlong),
                ("JobMemory", ctypes.c_ulonglong),
                ("JobMemoryLimit", ctypes.c_ulonglong),
                ("RateControlTolerance", ctypes.c_int),
                ("RateControlToleranceLimit", ctypes.c_int),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.QueryInformationJobObject.argtypes = [
            wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
            ctypes.POINTER(wintypes.DWORD),
        ]
        kernel32.QueryInformationJobObject.restype = wintypes.BOOL
        extended = EXTENDED_LIMIT()
        ok = kernel32.QueryInformationJobObject(
            self.handle, JOBOBJECTINFOCLASS_EXTENDED_LIMIT,
            ctypes.byref(extended), ctypes.sizeof(extended), None,
        )
        peak_process = int(extended.PeakProcessMemoryUsed) if ok else None
        peak_job = int(extended.PeakJobMemoryUsed) if ok else None
        violation = LIMIT_VIOLATION()
        violated = kernel32.QueryInformationJobObject(
            self.handle, JOBOBJECTINFOCLASS_LIMIT_VIOLATION,
            ctypes.byref(violation), ctypes.sizeof(violation), None,
        )
        flags = int(violation.ViolationLimitFlags) if violated else 0
        flags |= self.poll_messages(0)
        return peak_process, peak_job, flags


def _windows_process_alive(pid: int) -> bool:
    """Return whether an exact Windows PID still has a live process handle."""

    if not IS_WINDOWS or pid <= 0:
        return False
    try:
        import ctypes
        from ctypes import wintypes

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel32.OpenProcess.restype = wintypes.HANDLE
        kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel32.WaitForSingleObject.restype = wintypes.DWORD
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel32.OpenProcess(0x00100000 | 0x1000, False, pid)
        if not handle:
            return False
        try:
            return kernel32.WaitForSingleObject(handle, 0) == 0x102
        finally:
            kernel32.CloseHandle(handle)
    except Exception:
        return True


def _wait_for_windows_cleanup(
    root_pid: int,
    job: Optional[_WindowsJob],
    tracked_pids: set[int],
    timeout_s: float = WINDOWS_CLEANUP_GRACE_S,
) -> tuple[bool, list[int], list[int]]:
    """Wait boundedly for the Job and every exact observed descendant to exit."""

    deadline = time.monotonic() + timeout_s
    last_job_pids: list[int] = []
    last_live_pids: list[int] = []
    while True:
        if job is not None:
            last_job_pids = job.process_ids()
            tracked_pids.update(last_job_pids)
        tracked_pids.update(_windows_snapshot_descendants(root_pid))
        last_live_pids = sorted(pid for pid in tracked_pids if _windows_process_alive(pid))
        if not last_job_pids and not last_live_pids:
            return True, [], []
        if time.monotonic() >= deadline:
            return False, sorted(last_job_pids), last_live_pids
        time.sleep(min(WATCHDOG_TICK_S, 0.05))


def _create_windows_job(process_handle: int, limit_bytes: Optional[int]) -> tuple[Optional[_WindowsJob], str]:
    """Assign the exact child to a kill-on-close job or return a safe fallback reason."""

    if not IS_WINDOWS:
        return None, "not-windows"
    try:
        import ctypes
        from ctypes import wintypes

        class BASIC_LIMIT(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_longlong),
                ("PerJobUserTimeLimit", ctypes.c_longlong),
                ("LimitFlags", wintypes.DWORD),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t),
                ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class IO_COUNTERS(ctypes.Structure):
            _fields_ = [(name, ctypes.c_ulonglong) for name in (
                "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
                "ReadTransferCount", "WriteTransferCount", "OtherTransferCount",
            )]

        class EXTENDED_LIMIT(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", BASIC_LIMIT),
                ("IoInfo", IO_COUNTERS),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        class ASSOCIATE_COMPLETION_PORT(ctypes.Structure):
            _fields_ = [
                ("CompletionKey", ctypes.c_void_p),
                ("CompletionPort", wintypes.HANDLE),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        kernel32.CreateJobObjectW.restype = wintypes.HANDLE
        kernel32.SetInformationJobObject.argtypes = [
            wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
        ]
        kernel32.SetInformationJobObject.restype = wintypes.BOOL
        kernel32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        kernel32.AssignProcessToJobObject.restype = wintypes.BOOL
        kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel32.CreateIoCompletionPort.argtypes = [
            wintypes.HANDLE, wintypes.HANDLE, ctypes.c_size_t, wintypes.DWORD,
        ]
        kernel32.CreateIoCompletionPort.restype = wintypes.HANDLE

        handle = kernel32.CreateJobObjectW(None, None)
        if not handle:
            return None, f"CreateJobObjectW error={ctypes.get_last_error()}"
        info = EXTENDED_LIMIT()
        flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if limit_bytes is not None:
            flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY
            info.ProcessMemoryLimit = limit_bytes
            info.JobMemoryLimit = limit_bytes
        info.BasicLimitInformation.LimitFlags = flags
        if not kernel32.SetInformationJobObject(
            handle, JOBOBJECTINFOCLASS_EXTENDED_LIMIT,
            ctypes.byref(info), ctypes.sizeof(info),
        ):
            error = ctypes.get_last_error()
            kernel32.CloseHandle(handle)
            return None, f"SetInformationJobObject error={error}"
        invalid_handle = wintypes.HANDLE(-1).value
        completion_port = kernel32.CreateIoCompletionPort(invalid_handle, None, 0, 1)
        if not completion_port:
            error = ctypes.get_last_error()
            kernel32.CloseHandle(handle)
            return None, f"CreateIoCompletionPort error={error}"
        association = ASSOCIATE_COMPLETION_PORT(handle, completion_port)
        if not kernel32.SetInformationJobObject(
            handle, JOBOBJECTINFOCLASS_ASSOCIATE_COMPLETION_PORT,
            ctypes.byref(association), ctypes.sizeof(association),
        ):
            error = ctypes.get_last_error()
            kernel32.CloseHandle(completion_port)
            kernel32.CloseHandle(handle)
            return None, f"AssociateCompletionPort error={error}"
        if not kernel32.AssignProcessToJobObject(handle, process_handle):
            error = ctypes.get_last_error()
            kernel32.CloseHandle(completion_port)
            kernel32.CloseHandle(handle)
            return None, f"AssignProcessToJobObject error={error}"
        return _WindowsJob(int(handle), limit_bytes, int(completion_port)), ""
    except Exception as exc:
        return None, f"{type(exc).__name__}: {exc}"


def _aggregate_working_set_bytes(root_pid: int, job: Optional[_WindowsJob] = None) -> tuple[Optional[int], str, list[int]]:
    if IS_WINDOWS:
        job_pids = job.process_ids() if job is not None else []
        tree_pids = _windows_snapshot_descendants(root_pid)
        pids = sorted(set(job_pids) | set(tree_pids))
        source = (
            "job-process-list+toolhelp-descendant-snapshot"
            if job is not None
            else "toolhelp-descendant-snapshot"
        )
    else:
        pids = _posix_descendant_pids(root_pid)
        source = "proc-descendant-snapshot"
    samples = [get_working_set_bytes(pid) for pid in pids]
    present = [sample for sample in samples if sample is not None]
    return (sum(present) if present else None), source, pids


def _taskkill_tree(pid: int, timeout_s: float = TASKKILL_TIMEOUT_S) -> None:
    try:
        subprocess.run(
            ["taskkill", "/PID", str(pid), "/T", "/F"],
            capture_output=True, text=True, timeout=timeout_s,
        )
    except Exception:
        try:
            os.kill(pid, signal.SIGTERM)
        except Exception:
            pass


@dataclass
class WatchdogResult:
    verdict: str
    exit_code: Optional[int] = None
    duration_s: float = 0.0
    peak_ws_mb: Optional[float] = None
    peak_commit_mb: Optional[float] = None
    cpu_sec: Optional[float] = None
    isolation: str = "unknown"
    rss_sampling: str = "unavailable"
    stdout_path: Optional[str] = None
    stderr_path: Optional[str] = None
    detail: str = ""
    cleanup_verified: Optional[bool] = None
    cleanup_live_pids: list[int] = field(default_factory=list)


_WINDOWS_GATED_BOOTSTRAP = r"""
import json
import subprocess
import sys
import time
from pathlib import Path

command_path = Path(sys.argv[1])
gate_path = Path(sys.argv[2])
gate_deadline = time.monotonic() + 60.0
while not gate_path.is_file():
    if time.monotonic() >= gate_deadline:
        print("architecture-stress bootstrap gate timeout", file=sys.stderr)
        raise SystemExit(125)
    time.sleep(0.005)
payload = json.loads(command_path.read_text(encoding="utf-8"))
command_path.unlink(missing_ok=True)
command = payload.get("argv")
if not isinstance(command, list) or not command or not all(isinstance(v, str) for v in command):
    print("architecture-stress bootstrap received invalid argv", file=sys.stderr)
    raise SystemExit(126)
try:
    completed = subprocess.run(command, check=False)
except OSError as exc:
    print(f"architecture-stress bootstrap spawn failed: {exc}", file=sys.stderr)
    raise SystemExit(127)
raise SystemExit(completed.returncode)
"""


def watchdog_run(
    argv: Sequence[str],
    *,
    timeout_s: float = 30.0,
    rss_limit_mb: Optional[float] = None,
    cwd: str | os.PathLike | None = None,
    workdir: str | os.PathLike | None = None,
    label: str = "worker",
) -> WatchdogResult:
    """Run one worker with a wall deadline and optional memory ceiling.

    ``peak_ws_mb`` is the sampled sum of resident working sets for the current
    process tree.  The Windows Job Object limits are commit/private-memory
    backstops and are reported separately as ``peak_commit_mb``.
    """

    if timeout_s <= 0:
        raise ValueError("timeout_s must be positive")
    if rss_limit_mb is not None and rss_limit_mb <= 0:
        raise ValueError("rss_limit_mb must be positive")
    workdir = os.fspath(workdir) if workdir is not None else os.getcwd()
    run_dir = Path(tempfile.mkdtemp(prefix=f"a6_{label}_", dir=workdir))
    out_path = run_dir / "stdout.log"
    err_path = run_dir / "stderr.log"
    start = time.monotonic()
    deadline = start + timeout_s
    peak_ws_mb: Optional[float] = None
    peak_commit_mb: Optional[float] = None
    isolation = "unknown"
    rss_limit_bytes = int(rss_limit_mb * 1024**2) if rss_limit_mb is not None else None

    if IS_POSIX:
        isolation = "posix-session"
        with open(out_path, "wb") as outf, open(err_path, "wb") as errf:
            proc = subprocess.Popen(
                list(argv), cwd=cwd, stdout=outf, stderr=errf, start_new_session=True
            )
            last_cpu = None
            rss_sampling = "proc-descendant-snapshot"
            while True:
                code = proc.poll()
                ws, rss_sampling, _ = _aggregate_working_set_bytes(proc.pid)
                if ws is not None:
                    current_mb = ws / (1024**2)
                    peak_ws_mb = current_mb if peak_ws_mb is None else max(peak_ws_mb, current_mb)
                last_cpu = _posix_cpu_sec(proc.pid)
                if code is not None:
                    break
                verdict = None
                detail = ""
                if rss_limit_bytes is not None and ws is not None and ws >= rss_limit_bytes:
                    verdict = Verdict.RSS_KILL.value
                    detail = (
                        f"sampled aggregate working set {ws / 1024**2:.2f} MiB "
                        f">= {rss_limit_mb:.2f} MiB"
                    )
                elif time.monotonic() >= deadline:
                    verdict = Verdict.TIMEOUT.value
                    detail = f"exceeded {timeout_s:.1f}s deadline"
                if verdict is not None:
                    try:
                        os.killpg(proc.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    try:
                        proc.wait(timeout=POSIX_TERM_GRACE_S)
                    except subprocess.TimeoutExpired:
                        try:
                            os.killpg(proc.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        proc.wait()
                    code = proc.poll()
                    if code is None:
                        code = -signal.SIGKILL
                    _finalize(out_path, err_path, run_dir)
                    return WatchdogResult(
                        verdict=verdict, exit_code=code,
                        duration_s=time.monotonic() - start, peak_ws_mb=peak_ws_mb,
                        cpu_sec=last_cpu, isolation=isolation,
                        rss_sampling=rss_sampling,
                        stdout_path=str(out_path), stderr_path=str(err_path),
                        detail=detail + "; killpg(SIGTERM,SIGKILL)",
                    )
                time.sleep(WATCHDOG_TICK_S)
            exit_code = code
            duration = time.monotonic() - start
            if peak_ws_mb is None:
                peak_ws_mb = None
            _finalize(out_path, err_path, run_dir)
            return WatchdogResult(
                verdict=Verdict.PASS.value if exit_code == 0 else Verdict.FAIL.value,
                exit_code=exit_code, duration_s=duration, peak_ws_mb=peak_ws_mb,
                cpu_sec=last_cpu, isolation=isolation,
                rss_sampling=rss_sampling,
                stdout_path=str(out_path), stderr_path=str(err_path),
            )

    isolation = "windows-job-object"
    with open(out_path, "wb") as outf, open(err_path, "wb") as errf:
        command_path = run_dir / "command.json"
        gate_path = run_dir / "release.json"
        real_argv = [os.fsdecode(os.fspath(value)) for value in argv]
        atomic_write_json(command_path, {"argv": real_argv})
        creationflags = 0x00000200  # CREATE_NEW_PROCESS_GROUP
        proc = subprocess.Popen(
            [sys.executable, "-c", _WINDOWS_GATED_BOOTSTRAP,
             str(command_path), str(gate_path)],
            cwd=cwd, stdout=outf, stderr=errf,
            creationflags=creationflags,
        )
        job, job_error = _create_windows_job(int(proc._handle), rss_limit_bytes)
        if job is None:
            isolation = "windows-taskkill-fallback"
        try:
            # The bootstrap cannot execute real_argv until this atomic publish.
            # If Job assignment succeeded, every subsequently spawned process
            # inherits the Job before any user command can run.
            atomic_write_json(gate_path, {"released": True, "bootstrap_pid": proc.pid})
        except Exception:
            if job is not None:
                job.terminate(1)
                job.close()
            else:
                _taskkill_tree(proc.pid)
            try:
                proc.wait(timeout=TASKKILL_TIMEOUT_S)
            except subprocess.TimeoutExpired:
                proc.kill()
            raise
        rss_sampling = "job-process-list" if job is not None else "toolhelp-descendant-snapshot"
        last_cpu = None
        tracked_pids = {proc.pid}
        try:
            while True:
                code = proc.poll()
                ws, rss_sampling, sampled_pids = _aggregate_working_set_bytes(proc.pid, job)
                tracked_pids.update(sampled_pids)
                if ws is not None:
                    current_mb = ws / (1024**2)
                    peak_ws_mb = current_mb if peak_ws_mb is None else max(peak_ws_mb, current_mb)
                if code is not None:
                    break
                verdict = None
                detail = ""
                if rss_limit_bytes is not None and ws is not None and ws >= rss_limit_bytes:
                    verdict = Verdict.RSS_KILL.value
                    detail = (
                        f"sampled aggregate working set {ws / 1024**2:.2f} MiB "
                        f">= {rss_limit_mb:.2f} MiB"
                    )
                elif time.monotonic() >= deadline:
                    verdict = Verdict.TIMEOUT.value
                    detail = f"exceeded {timeout_s:.1f}s deadline"
                if verdict is not None:
                    tracked_pids.update(_windows_snapshot_descendants(proc.pid))
                    if job is not None:
                        tracked_pids.update(job.process_ids())
                    if job is not None:
                        job.terminate(1)
                        kill_method = "TerminateJobObject"
                    else:
                        _taskkill_tree(proc.pid)
                        kill_method = "taskkill /PID /T fallback"
                    try:
                        proc.wait(timeout=TASKKILL_TIMEOUT_S)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait(timeout=TASKKILL_TIMEOUT_S)
                    if job is not None:
                        _, peak_commit, _ = job.accounting()
                        peak_commit_mb = peak_commit / 1024**2 if peak_commit is not None else None
                    cleanup_ok, job_pids, live_pids = _wait_for_windows_cleanup(
                        proc.pid, job, tracked_pids
                    )
                    cleanup_detail = "; cleanup_verified=true"
                    result_isolation = isolation
                    if not cleanup_ok:
                        result_isolation += "-cleanup-failed"
                        cleanup_detail = (
                            "; cleanup_verified=false"
                            f"; active_job_pids={job_pids}; live_tracked_pids={live_pids}"
                        )
                    return WatchdogResult(
                        verdict=verdict, exit_code=proc.poll(),
                        duration_s=time.monotonic() - start,
                        peak_ws_mb=peak_ws_mb, peak_commit_mb=peak_commit_mb,
                        cpu_sec=last_cpu, isolation=result_isolation,
                        rss_sampling=rss_sampling,
                        stdout_path=str(out_path), stderr_path=str(err_path),
                        detail=f"{detail}; {kill_method}"
                        + (f"; fallback_reason={job_error}" if job is None else "")
                        + cleanup_detail,
                        cleanup_verified=cleanup_ok,
                        cleanup_live_pids=live_pids,
                    )
                time.sleep(WATCHDOG_TICK_S)

            exit_code = code
            violation_flags = 0
            if job is not None:
                _, peak_commit, violation_flags = job.accounting()
                peak_commit_mb = peak_commit / 1024**2 if peak_commit is not None else None
            commit_hit = bool(violation_flags & (JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY))
            if (
                not commit_hit
                and exit_code != 0
                and rss_limit_bytes is not None
                and peak_commit_mb is not None
                and peak_commit_mb * 1024**2 >= int(rss_limit_bytes * 0.98)
            ):
                commit_hit = True
            verdict = (
                Verdict.COMMIT_KILL.value
                if commit_hit
                else Verdict.PASS.value if exit_code == 0 else Verdict.FAIL.value
            )
            detail = ""
            if commit_hit:
                detail = (
                    f"Job commit/private-memory limit reached; violation_flags=0x{violation_flags:x}; "
                    "this is not an RSS measurement"
                )
            elif job is None:
                detail = f"Job Object unavailable; safe fallback active: {job_error}"
            return WatchdogResult(
                verdict=verdict, exit_code=exit_code,
                duration_s=time.monotonic() - start,
                peak_ws_mb=peak_ws_mb, peak_commit_mb=peak_commit_mb,
                cpu_sec=last_cpu, isolation=isolation,
                rss_sampling=rss_sampling,
                stdout_path=str(out_path), stderr_path=str(err_path), detail=detail,
            )
        finally:
            # KILL_ON_JOB_CLOSE guarantees no assigned descendant survives.
            if job is not None:
                job.close()
            _finalize(out_path, err_path, run_dir)


def _posix_cpu_sec(pid: int) -> Optional[float]:
    try:
        with open(f"/proc/{pid}/stat", "r", encoding="utf-8") as f:
            fields = f.read().split()
        ticks = int(fields[13]) + int(fields[14])
        clk = os.sysconf("SC_CLK_TCK")
        return ticks / clk if clk else None
    except Exception:
        return None


def _finalize(out_path: Path, err_path: Path, run_dir: Path) -> None:
    pass


def synthesize_absent_result(
    run_id: str, test_id: str, watchdog: WatchdogResult,
    wave: Optional[int] = None, lane: Optional[str] = None,
) -> ResultRecord:
    return ResultRecord(
        run_id=run_id, test_id=test_id,
        verdict=watchdog.verdict if watchdog.verdict in {
            Verdict.TIMEOUT.value, Verdict.RSS_KILL.value, Verdict.COMMIT_KILL.value
        }
        else Verdict.CRASH.value,
        isolation=watchdog.isolation,
        duration_s=watchdog.duration_s,
        exit_code=watchdog.exit_code,
        peak_ws_mb=watchdog.peak_ws_mb,
        peak_commit_mb=watchdog.peak_commit_mb,
        cpu_sec=watchdog.cpu_sec,
        detail=f"worker produced no result file; watchdog synthesis: {watchdog.detail or 'exit without publish'}",
        wave=wave, lane=lane,
        measured={
            "stdout_path": watchdog.stdout_path,
            "stderr_path": watchdog.stderr_path,
            "rss_sampling": watchdog.rss_sampling,
        },
    )


class ResultAggregator:
    def __init__(self, run_id: Optional[str] = None) -> None:
        self.run_id = run_id or new_run_id()
        self._records: dict[tuple[str, str], dict] = {}
        self.duplicates: list[dict] = []

    def add(self, record: Mapping[str, Any]) -> list[str]:
        rec = dict(record)
        rec.setdefault("run_id", self.run_id)
        errors = validate_record(rec)
        if errors:
            return errors
        key = (str(rec["run_id"]), str(rec["test_id"]))
        if key in self._records:
            self.duplicates.append(rec)
            return [f"duplicate (run_id, test_id): {key}"]
        self._records[key] = rec
        return []

    def add_result(self, result: ResultRecord) -> list[str]:
        return self.add(result.to_dict())

    def _sort_key(self, rec: dict) -> tuple:
        parts = []
        for key in ORDER_KEYS:
            val = rec.get(key)
            if key == "wave":
                parts.append(val if isinstance(val, int) else -1)
            elif key == "lane":
                parts.append(str(val or ""))
            elif key == "run_id":
                parts.append(str(val or ""))
            else:
                parts.append(str(val or ""))
        return tuple(parts)

    def records(self) -> list[dict]:
        return [self._records[k] for k in sorted(self._records, key=lambda k: self._sort_key(self._records[k]))]

    def publish(self, path: str | os.PathLike) -> dict:
        recs = self.records()
        lines = [json.dumps({"type": "run_header", "run_id": self.run_id,
                             "ts": utcnow_iso(), "schema_version": SCHEMA_VERSION,
                             "count": len(recs)}, ensure_ascii=False, sort_keys=True)]
        for rec in recs:
            lines.append(json.dumps(rec, ensure_ascii=False, sort_keys=True))
        lines.append(json.dumps({"type": "run_footer", "run_id": self.run_id,
                                 "records": len(recs), "duplicates": len(self.duplicates)},
                                ensure_ascii=False, sort_keys=True))
        p = Path(path)
        p.parent.mkdir(parents=True, exist_ok=True)
        tmp = p.with_name(p.name + ".tmp")
        with open(tmp, "w", encoding="utf-8") as f:
            for line in lines:
                f.write(line + "\n")
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, p)
        return {"published": str(p), "records": len(recs), "duplicates": len(self.duplicates),
                "run_id": self.run_id}


def load_jsonl(path: str | os.PathLike) -> list[dict]:
    records = []
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                records.append({"type": "corrupt_line", "raw": line[:500]})
    return records
