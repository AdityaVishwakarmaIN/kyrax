"""Deterministic fixture registry and small generation helpers for A6.

The canonical registry describes F01--F12 without pretending that the large or
specialized corpora already exist.  Missing generators are explicitly BLOCKED;
an incomplete registry cannot be exported as a ready fixture manifest.

Only the tiny self-test generators in this module write files.  Every generated
output is confined to a resolved run root and committed with a unique atomic
temporary file.  No import-time generation occurs.
"""

from __future__ import annotations

import io
import json
import os
import random
import re
import shutil
import tempfile
import zipfile
from contextlib import contextmanager
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any, Iterator, Mapping, Sequence

try:
    from .common import resource_preflight, sha256_file, zip_member_manifest
except ImportError:  # Direct import during focused harness checks.
    from common import resource_preflight, sha256_file, zip_member_manifest  # type: ignore[no-redef]


DEFAULT_SEED = 42
TEMP_PREFIX = "kyrax_archstress_"
PROJECT_ROOT = Path(__file__).resolve().parents[2]

STATE_READY = "READY"
STATE_UNMATERIALIZED = "UNMATERIALIZED"
STATE_BLOCKED = "BLOCKED"
VALID_STATES = frozenset({STATE_READY, STATE_UNMATERIALIZED, STATE_BLOCKED})
VALID_CLEANUP_POLICIES = frozenset({"run-temp", "preserve", "source-never-delete"})
VALID_ARTIFACT_FORMATS = frozenset({"zip", "binary", "text", "malformed-zip", "cfb"})
VALID_ARTIFACT_ORIGINS = frozenset({"source", "generated", "self-test"})
FIXTURE_ID_RE = re.compile(r"^[A-Za-z][A-Za-z0-9]*(?:-[A-Za-z0-9]+)*$")


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _json_mapping(value: Mapping[str, Any], field_name: str) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise TypeError(f"{field_name} must be a mapping")
    copied = dict(value)
    if not all(isinstance(key, str) and key for key in copied):
        raise TypeError(f"{field_name} keys must be non-empty strings")
    try:
        json.dumps(copied, sort_keys=True)
    except (TypeError, ValueError) as exc:
        raise TypeError(f"{field_name} must be JSON-serializable") from exc
    return copied


def _normalized_features(features: Sequence[str]) -> tuple[str, ...]:
    if isinstance(features, (str, bytes)) or not isinstance(features, Sequence):
        raise TypeError("features must be a sequence of feature-name strings")
    normalized: list[str] = []
    for item in features:
        if not isinstance(item, str) or not item.strip():
            raise TypeError("features must contain non-empty strings")
        normalized.append(item.strip())
    if len(set(normalized)) != len(normalized):
        raise ValueError("features must not contain duplicates")
    return tuple(sorted(normalized))


def _path_value(value: Path | None, field_name: str) -> Path | None:
    if value is not None and not isinstance(value, Path):
        raise TypeError(f"{field_name} must be pathlib.Path or None")
    return value


def _contained_path(run_root: Path, path: Path) -> Path:
    root = run_root.resolve(strict=True)
    if not root.is_dir():
        raise ValueError(f"run_root is not a directory: {root}")
    candidate = path if path.is_absolute() else root / path
    candidate = candidate.resolve(strict=False)
    try:
        candidate.relative_to(root)
    except ValueError as exc:
        raise ValueError(f"generated output escapes run root: {candidate}") from exc
    if candidate == root:
        raise ValueError("generated output must name a file below run_root")
    return candidate


@dataclass(frozen=True)
class FixtureArtifact:
    name: str
    role: str
    path: Path
    expected_format: str = "zip"
    origin: str = "source"
    required: bool = True
    run_root: Path | None = None
    expected_sha256: str | None = None

    def validate(self) -> None:
        if not isinstance(self.name, str) or not FIXTURE_ID_RE.fullmatch(self.name):
            raise ValueError(f"invalid artifact name: {self.name!r}")
        if not isinstance(self.role, str) or not self.role.strip():
            raise ValueError("artifact role must be a non-empty string")
        _path_value(self.path, "artifact.path")
        _path_value(self.run_root, "artifact.run_root")
        if self.expected_format not in VALID_ARTIFACT_FORMATS:
            raise ValueError(f"invalid artifact format: {self.expected_format!r}")
        if self.origin not in VALID_ARTIFACT_ORIGINS:
            raise ValueError(f"invalid artifact origin: {self.origin!r}")
        if not isinstance(self.required, bool):
            raise TypeError("artifact.required must be bool")
        if self.expected_sha256 is not None and not re.fullmatch(
            r"[0-9a-fA-F]{64}", self.expected_sha256
        ):
            raise ValueError("artifact.expected_sha256 must be a 64-digit hex digest")
        if self.origin == "generated":
            if self.run_root is None:
                raise ValueError("generated artifact requires run_root")
            _contained_path(self.run_root, self.path)


@dataclass(frozen=True)
class FixtureSpec:
    fixture_id: str
    generator_command: str | None = None
    seed: int = DEFAULT_SEED
    dimensions: Mapping[str, Any] = field(default_factory=dict)
    features: Sequence[str] = field(default_factory=tuple)
    owner: str = "A6"
    cleanup_policy: str = "run-temp"
    required_bytes: int = 0
    path: Path | None = None  # Backward-compatible single source artifact.
    artifacts: Sequence[FixtureArtifact] = field(default_factory=tuple)
    state: str = STATE_UNMATERIALIZED
    blocked_reason: str | None = None

    def validate(self) -> None:
        if not isinstance(self.fixture_id, str) or not FIXTURE_ID_RE.fullmatch(self.fixture_id):
            raise ValueError(f"invalid fixture id: {self.fixture_id!r}")
        if self.generator_command is not None:
            if not isinstance(self.generator_command, str) or not self.generator_command.strip():
                raise ValueError("generator_command must be None or a non-empty string")
            if "{output}" not in self.generator_command or "{seed}" not in self.generator_command:
                raise ValueError("generator_command must include {output} and {seed}")
        if not _is_int(self.seed) or self.seed < 0:
            raise ValueError("seed must be a non-negative integer")
        if not _is_int(self.required_bytes) or self.required_bytes < 0:
            raise ValueError("required_bytes must be a non-negative integer")
        _json_mapping(self.dimensions, "dimensions")
        _normalized_features(self.features)
        if not isinstance(self.owner, str) or not self.owner.strip():
            raise ValueError("owner must be a non-empty string")
        if self.cleanup_policy not in VALID_CLEANUP_POLICIES:
            raise ValueError(f"invalid cleanup policy: {self.cleanup_policy!r}")
        _path_value(self.path, "path")
        if isinstance(self.artifacts, (str, bytes)) or not isinstance(self.artifacts, Sequence):
            raise TypeError("artifacts must be a sequence of FixtureArtifact")
        names: set[str] = set()
        for artifact in self.artifacts:
            if not isinstance(artifact, FixtureArtifact):
                raise TypeError("artifacts must contain FixtureArtifact values")
            artifact.validate()
            if artifact.name in names:
                raise ValueError(f"duplicate artifact name: {artifact.name}")
            names.add(artifact.name)
        if self.state not in VALID_STATES:
            raise ValueError(f"invalid fixture state: {self.state!r}")
        if self.state == STATE_BLOCKED and not (
            isinstance(self.blocked_reason, str) and self.blocked_reason.strip()
        ):
            raise ValueError("BLOCKED fixture requires blocked_reason")
        if self.blocked_reason is not None and not isinstance(self.blocked_reason, str):
            raise TypeError("blocked_reason must be str or None")
        if self.state == STATE_READY and not self._all_artifacts():
            raise ValueError("READY fixture requires at least one artifact")

    def _all_artifacts(self) -> tuple[FixtureArtifact, ...]:
        values = list(self.artifacts)
        if self.path is not None:
            values.insert(
                0,
                FixtureArtifact(
                    name="primary",
                    role="primary",
                    path=self.path,
                    expected_format="zip",
                    origin="source",
                ),
            )
        return tuple(values)

    def with_path(self, path: str | os.PathLike[str]) -> "FixtureSpec":
        if isinstance(path, bytes) or not isinstance(path, (str, os.PathLike)):
            raise TypeError("path must be str or os.PathLike")
        return replace(self, path=Path(path))

    def with_artifacts(
        self,
        artifacts: Sequence[FixtureArtifact],
        *,
        state: str | None = None,
    ) -> "FixtureSpec":
        return replace(self, artifacts=tuple(artifacts), state=state or self.state)


def _artifact_record(artifact: FixtureArtifact) -> dict[str, Any]:
    artifact.validate()
    path = artifact.path.resolve(strict=False)
    record: dict[str, Any] = {
        "name": artifact.name,
        "role": artifact.role,
        "path": str(path),
        "format": artifact.expected_format,
        "origin": artifact.origin,
        "required": artifact.required,
        "present": path.is_file(),
        "sha256": None,
        "expected_sha256": artifact.expected_sha256.lower() if artifact.expected_sha256 else None,
        "hash_matches_expected": None,
        "size_compressed": None,
        "size_expanded": None,
        "inspection_status": "MISSING",
    }
    if not path.is_file():
        return record

    size = path.stat().st_size
    actual_sha256 = sha256_file(path)
    record.update(
        {
            "sha256": actual_sha256,
            "hash_matches_expected": (
                actual_sha256.lower() == artifact.expected_sha256.lower()
                if artifact.expected_sha256
                else None
            ),
            "size_compressed": size,
            "inspection_status": "PRESENT",
        }
    )
    if artifact.expected_format in {"binary", "text", "cfb"}:
        record["size_expanded"] = size
        record["inspection_status"] = "NON-ZIP"
        return record

    try:
        inventory = zip_member_manifest(path)
    except (zipfile.BadZipFile, zipfile.LargeZipFile, OSError, RuntimeError, ValueError) as exc:
        record["zip_error"] = f"{type(exc).__name__}: {exc}"
        if artifact.expected_format == "malformed-zip":
            record["inspection_status"] = "EXPECTED-MALFORMED"
            return record
        record["inspection_status"] = "ZIP-ERROR"
        return record

    record["zip"] = inventory
    record["size_expanded"] = sum(int(member["size"]) for member in inventory["members"])
    record["inspection_status"] = "ZIP-OK"
    if artifact.expected_format == "malformed-zip":
        record["inspection_status"] = "UNEXPECTED-VALID-ZIP"
    return record


def manifest_record(spec: FixtureSpec) -> dict[str, Any]:
    """Return a manifest record with explicit readiness and artifact status."""

    spec.validate()
    artifact_records = [_artifact_record(item) for item in spec._all_artifacts()]
    errors: list[str] = []
    required = [item for item in artifact_records if item["required"]]
    if not required:
        errors.append("no required artifacts declared")
    for item in required:
        if not item["present"]:
            errors.append(f"missing required artifact: {item['name']}")
        elif item["sha256"] is None or item["size_compressed"] is None:
            errors.append(f"incomplete artifact metadata: {item['name']}")
        elif item["hash_matches_expected"] is False:
            errors.append(f"pinned hash mismatch: {item['name']}")
        elif item["inspection_status"] in {"ZIP-ERROR", "UNEXPECTED-VALID-ZIP"}:
            errors.append(f"artifact inspection failed: {item['name']}")

    if spec.state == STATE_BLOCKED:
        readiness = STATE_BLOCKED
    elif spec.state == STATE_UNMATERIALIZED:
        readiness = STATE_UNMATERIALIZED
    elif errors:
        readiness = STATE_BLOCKED
    else:
        readiness = STATE_READY

    features = _normalized_features(spec.features)
    record: dict[str, Any] = {
        "id": spec.fixture_id,
        "generator_command": spec.generator_command,
        "seed": spec.seed,
        "dimensions": _json_mapping(spec.dimensions, "dimensions"),
        "features": list(features),
        "feature_inventory": {"declared": list(features), "status": "DECLARED"},
        "owner": spec.owner,
        "cleanup_policy": spec.cleanup_policy,
        "required_bytes": spec.required_bytes,
        "state": spec.state,
        "readiness": readiness,
        "blocked_reason": spec.blocked_reason,
        "present": bool(required) and all(item["present"] for item in required),
        "artifacts": artifact_records,
        "readiness_errors": errors,
    }
    if len(artifact_records) == 1:
        only = artifact_records[0]
        record.update(
            {
                "path": only["path"],
                "sha256": only["sha256"],
                "size_compressed": only["size_compressed"],
                "size_expanded": only["size_expanded"],
            }
        )
        if "zip" in only:
            record["zip"] = only["zip"]
    return record


class FixtureRegistry:
    def __init__(self) -> None:
        self._specs: dict[str, FixtureSpec] = {}

    def register(self, spec: FixtureSpec) -> None:
        if not isinstance(spec, FixtureSpec):
            raise TypeError("registry accepts FixtureSpec values")
        spec.validate()
        if spec.fixture_id in self._specs:
            raise ValueError(f"duplicate fixture id: {spec.fixture_id}")
        self._specs[spec.fixture_id] = spec

    def get(self, fixture_id: str) -> FixtureSpec:
        return self._specs[fixture_id]

    def records(self) -> list[dict[str, Any]]:
        return [manifest_record(self._specs[key]) for key in sorted(self._specs)]

    def validate_complete(self) -> list[str]:
        errors: list[str] = []
        for record in self.records():
            if record["readiness"] != STATE_READY:
                detail = record["blocked_reason"] or "; ".join(record["readiness_errors"])
                errors.append(f"{record['id']}: {record['readiness']} ({detail or 'not ready'})")
        return errors

    def preflight(
        self,
        fixture_id: str,
        root: str | os.PathLike[str],
        *,
        output_bytes: int = 0,
        copies: int = 2,
        headroom: float = 1.2,
    ) -> tuple[bool, str]:
        spec = self.get(fixture_id)
        if not _is_int(output_bytes) or output_bytes < 0:
            raise ValueError("output_bytes must be a non-negative integer")
        if not _is_int(copies) or copies < 2:
            raise ValueError("copies must be an integer >= 2")
        if not isinstance(headroom, (int, float)) or isinstance(headroom, bool) or headroom < 1.2:
            raise ValueError("headroom must be numeric and >= 1.2")
        # common.resource_preflight remains the shared disk-measurement API.  Pass
        # the original input plus two additional copies explicitly; output is
        # separate.  This removes the old ambiguity where copies=2 meant only 2x.
        return resource_preflight(
            root,
            need_input_bytes=spec.required_bytes,
            need_output_bytes=output_bytes,
            copies=copies + 1,
            headroom=headroom,
        )

    def export(
        self,
        path: str | os.PathLike[str],
        *,
        allow_incomplete: bool = False,
    ) -> Path:
        errors = self.validate_complete()
        if errors and not allow_incomplete:
            raise ValueError("fixture registry is not ready: " + " | ".join(errors))
        target = Path(path)
        _atomic_write_json(
            target,
            {
                "schema_version": 2,
                "ready": not errors,
                "validation_errors": errors,
                "fixtures": self.records(),
            },
        )
        return target


@contextmanager
def run_temp_root(
    base_dir: str | os.PathLike[str] | None = None,
    *,
    cleanup: bool = True,
) -> Iterator[Path]:
    """Yield a uniquely named resolved run directory and remove only it."""

    if isinstance(base_dir, bytes):
        raise TypeError("base_dir must be str, os.PathLike, or None")
    root = Path(tempfile.mkdtemp(prefix=TEMP_PREFIX, dir=base_dir)).resolve()
    try:
        yield root
    finally:
        if cleanup and root.name.startswith(TEMP_PREFIX) and root.is_dir():
            shutil.rmtree(root)


def _unique_temp_path(target: Path) -> Path:
    target.parent.mkdir(parents=True, exist_ok=True)
    handle, name = tempfile.mkstemp(prefix=f".{target.name}.", suffix=".tmp", dir=target.parent)
    os.close(handle)
    return Path(name)


def _atomic_write_bytes(path: Path, payload: bytes) -> None:
    temp = _unique_temp_path(path)
    try:
        with open(temp, "wb") as stream:
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp, path)
    finally:
        if temp.exists():
            temp.unlink()


def _atomic_write_json(path: Path, payload: Mapping[str, Any]) -> None:
    temp = _unique_temp_path(path)
    try:
        with open(temp, "w", encoding="utf-8") as stream:
            json.dump(payload, stream, ensure_ascii=False, indent=2, sort_keys=True)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temp, path)
    finally:
        if temp.exists():
            temp.unlink()


def generate_small_binary(
    path: str | os.PathLike[str],
    *,
    run_root: str | os.PathLike[str],
    seed: int = DEFAULT_SEED,
    size: int = 4096,
) -> Path:
    if not _is_int(seed) or seed < 0:
        raise ValueError("seed must be a non-negative integer")
    if not _is_int(size) or size < 0:
        raise ValueError("size must be a non-negative integer")
    root = Path(run_root)
    target = _contained_path(root, Path(path))
    rng = random.Random(seed)
    payload = bytes(rng.randrange(256) for _ in range(size))
    _atomic_write_bytes(target, payload)
    return target


def generate_small_text(
    path: str | os.PathLike[str],
    *,
    run_root: str | os.PathLike[str],
    seed: int = DEFAULT_SEED,
    rows: int = 32,
) -> Path:
    if not _is_int(seed) or seed < 0:
        raise ValueError("seed must be a non-negative integer")
    if not _is_int(rows) or rows < 0:
        raise ValueError("rows must be a non-negative integer")
    root = Path(run_root)
    target = _contained_path(root, Path(path))
    rng = random.Random(seed)
    text = "\n".join(f"{index},{rng.randrange(1_000_000)}" for index in range(rows))
    _atomic_write_bytes(target, (text + "\n").encode("utf-8"))
    return target


def _blocked(
    fixture_id: str,
    *,
    dimensions: Mapping[str, Any],
    features: Sequence[str],
    reason: str,
    required_bytes: int = 0,
) -> FixtureSpec:
    return FixtureSpec(
        fixture_id=fixture_id,
        generator_command=None,
        dimensions=dimensions,
        features=features,
        required_bytes=required_bytes,
        state=STATE_BLOCKED,
        blocked_reason=reason,
    )


F01_SHA256 = "40c91c34437cfe8ff0c61f2c8d685e2aa597e0fe54f1376d0c8cc76220f626ca"


CANONICAL_SPECS: tuple[FixtureSpec, ...] = (
    FixtureSpec(
        "F01",
        dimensions={"source": "testdata/structured.xlsx", "purpose": "hang-reproducer"},
        features=("structured-workbook",),
        cleanup_policy="source-never-delete",
        artifacts=(
            FixtureArtifact(
                "primary",
                "pinned-source",
                PROJECT_ROOT / "testdata" / "structured.xlsx",
                expected_sha256=F01_SHA256,
            ),
        ),
        state=STATE_READY,
    ),
    _blocked(
        "F02",
        dimensions={"corpus": "formula-cache", "families": "all"},
        features=("cached-values", "errors", "formulas", "shared-formulas"),
        reason="formula/cache corpus generator is not implemented",
    ),
    _blocked(
        "F03",
        dimensions={"cells": 1, "shape": "one-cell-everything"},
        features=("comments", "conditional-formatting", "data-validation", "filters", "formulas", "hyperlinks", "merges", "styles", "tables"),
        reason="one-cell-everything generator is not implemented",
    ),
    _blocked(
        "F04",
        dimensions={"corpus": "ooxml-features"},
        features=("charts", "formatting", "images", "legacy-comments", "pivots", "threaded-comments", "validations"),
        reason="OOXML feature corpus generator is not implemented",
    ),
    _blocked(
        "F05",
        dimensions={"corpus": "preservation"},
        features=("controls", "external-links", "power-query", "rich-values", "signatures", "slicers", "sparklines"),
        reason="preservation corpus sources/generator are not implemented",
    ),
    _blocked(
        "F06",
        dimensions={"corpus": "mutation-grid", "bands": ("before", "inside", "across", "after")},
        features=("conditional-formatting", "data-validation", "drawing-anchors", "filters", "formulas", "merges", "names", "tables"),
        reason="mutation-grid generator is not implemented",
    ),
    _blocked(
        "F07",
        dimensions={"max_row": 1_048_576, "max_column": "XFD", "variants": ("sparse", "dense")},
        features=("boundary-grid",),
        reason="boundary-grid generator is not implemented",
    ),
    _blocked(
        "F08",
        dimensions={"string_lengths": (32_766, 32_767, 32_768), "unique_fractions": ("below-0.5", "at-0.5", "above-0.5")},
        features=("shared-strings", "string-boundary"),
        reason="string/SST boundary generator is not implemented",
    ),
    _blocked(
        "F09",
        dimensions={"classes": ("small-report", "medium-styled", "formula-heavy", "image-heavy", "1m-by-10"), "max_cells": 10_000_000},
        features=("northstar",),
        reason="northstar generators are not implemented; large generation must remain opt-in",
        required_bytes=800_000_000,
    ),
    _blocked(
        "F10",
        dimensions={"corpus": "malformed", "variants": ("truncated-zip", "truncated-xml", "bad-crc", "duplicate-names", "dangling-rels", "invalid-utf-xml")},
        features=("malformed-containers",),
        reason="malformed multi-artifact corpus generator is not implemented",
    ),
    _blocked(
        "F11",
        dimensions={"entry_counts": (65_535, 65_536, 65_537), "sentinel": ("zip64-offset", "zip64-size"), "real_minimum_bytes": 4 * 1024**3 + 1},
        features=("zip64",),
        reason="Zip64 entry-count, sentinel, and >4 GiB generators are not implemented",
        required_bytes=4 * 1024**3 + 1,
    ),
    _blocked(
        "F12",
        dimensions={"corpus": "agile-aes", "password_cases": ("correct", "wrong", "empty"), "spin_counts": ("below-max", "at-max", "over-max")},
        features=("encryption",),
        reason="encrypted CFB corpus sources/generator are not implemented",
    ),
)

F09_SPECS: tuple[FixtureSpec, ...] = (CANONICAL_SPECS[8],)
F11_SPECS: tuple[FixtureSpec, ...] = (CANONICAL_SPECS[10],)


def canonical_specs() -> tuple[FixtureSpec, ...]:
    return CANONICAL_SPECS


def canonical_registry() -> FixtureRegistry:
    registry = FixtureRegistry()
    for spec in CANONICAL_SPECS:
        registry.register(spec)
    return registry


def scale_specs() -> tuple[FixtureSpec, ...]:
    return F09_SPECS + F11_SPECS


def _deterministic_zip_bytes() -> bytes:
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_STORED) as archive:
        info = zipfile.ZipInfo("payload.txt", date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_STORED
        archive.writestr(info, b"fixture-self-test\n")
    return stream.getvalue()


def self_test(temp_dir: str | os.PathLike[str]) -> dict[str, Any]:
    """Exercise ZIP, non-ZIP, malformed, multi-artifact, and gate behavior."""

    root = Path(temp_dir).resolve(strict=True)
    if not root.is_dir():
        raise ValueError("self_test temp_dir must be an existing directory")

    first = generate_small_binary("a.bin", run_root=root, seed=DEFAULT_SEED, size=1024)
    second = generate_small_binary("b.bin", run_root=root, seed=DEFAULT_SEED, size=1024)
    if sha256_file(first) != sha256_file(second):
        raise AssertionError("same-seed fixtures are not deterministic")

    invalid_specs = {
        "id": FixtureSpec("not valid"),
        "command": FixtureSpec("bad-command", generator_command="tool --output {output}"),
        "seed": FixtureSpec("bad-seed", seed=True),
        "bytes": FixtureSpec("bad-bytes", required_bytes=-1),
        "features": FixtureSpec("bad-features", features="not-a-sequence"),
        "cleanup": FixtureSpec("bad-cleanup", cleanup_policy="delete-anything"),
        "path": FixtureSpec("bad-path", path="not-a-Path"),  # type: ignore[arg-type]
    }
    rejected_fields: list[str] = []
    for field_name, invalid in invalid_specs.items():
        try:
            invalid.validate()
        except (TypeError, ValueError):
            rejected_fields.append(field_name)
        else:
            raise AssertionError(f"invalid {field_name} was accepted")

    valid_zip = _contained_path(root, Path("valid.zip"))
    malformed_zip = _contained_path(root, Path("malformed.zip"))
    _atomic_write_bytes(valid_zip, _deterministic_zip_bytes())
    _atomic_write_bytes(malformed_zip, b"PK\x03\x04truncated")

    artifacts = (
        FixtureArtifact("zip", "valid-zip", valid_zip, "zip", "self-test"),
        FixtureArtifact("binary", "non-zip", first, "binary", "self-test"),
        FixtureArtifact("malformed", "expected-malformed", malformed_zip, "malformed-zip", "self-test"),
    )
    registry = FixtureRegistry()
    registry.register(
        FixtureSpec(
            "self-multi",
            dimensions={"artifacts": 3},
            features=("self-test",),
            artifacts=artifacts,
            state=STATE_READY,
        )
    )
    manifest_path = registry.export(root / "manifest.json")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    record = manifest["fixtures"][0]
    statuses = {item["name"]: item["inspection_status"] for item in record["artifacts"]}
    expected = {"zip": "ZIP-OK", "binary": "NON-ZIP", "malformed": "EXPECTED-MALFORMED"}
    if statuses != expected:
        raise AssertionError(f"unexpected artifact statuses: {statuses!r}")

    canonical = canonical_registry()
    blocked_errors = canonical.validate_complete()
    if len(blocked_errors) != 11:
        raise AssertionError(f"canonical readiness must expose 11 blockers: {blocked_errors!r}")
    try:
        canonical.export(root / "must-not-publish-ready.json")
    except ValueError:
        pass
    else:
        raise AssertionError("incomplete canonical registry published as ready")
    planning_path = canonical.export(root / "planning.json", allow_incomplete=True)
    planning = json.loads(planning_path.read_text(encoding="utf-8"))
    if planning["ready"] is not False:
        raise AssertionError("planning manifest must explicitly report ready=false")

    try:
        generate_small_text("../escape.txt", run_root=root)
    except ValueError:
        pass
    else:
        raise AssertionError("run-root escape was not rejected")

    return {
        "fixture_count": len(manifest["fixtures"]),
        "artifact_count": len(record["artifacts"]),
        "artifact_statuses": statuses,
        "validation_rejections": rejected_fields,
        "canonical_blocked": len(blocked_errors),
        "manifest": str(manifest_path),
        "planning_manifest": str(planning_path),
        "sha256": sha256_file(first),
    }
