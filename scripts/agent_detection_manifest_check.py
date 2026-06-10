#!/usr/bin/env python3
"""Validate bundled and published agent detection manifests."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BUNDLED_DIR = PROJECT_ROOT / "src" / "detect" / "manifests"
DEFAULT_WEBSITE_DIR = PROJECT_ROOT / "website" / "agent-detection"
ENGINE_SOURCE = PROJECT_ROOT / "src" / "detect" / "manifest_update.rs"

MANIFEST_KEYS = {"id", "version", "min_engine_version", "updated_at", "aliases", "rules"}
RULE_KEYS = {
    "id",
    "state",
    "priority",
    "region",
    "visible_idle",
    "visible_blocker",
    "visible_working",
    "skip_state_update",
    "all",
    "any",
    "not",
    "contains",
    "regex",
    "line_regex",
}
GATE_KEYS = {"all", "any", "not", "contains", "regex", "line_regex"}
STATES = {"idle", "working", "blocked", "unknown"}
REGION_RE = re.compile(
    r"^(whole_recent|whole_recent_without_current_prompt_marker|after_last_prompt_marker|"
    r"before_current_prompt_marker|current_prompt_block_marker|after_current_prompt_block_marker|"
    r"prompt_box_body|above_prompt_box|last_non_empty_above_prompt_box|after_last_horizontal_rule|"
    r"osc_title|osc_progress|"
    r"bottom_lines\([1-9][0-9]*\)|bottom_non_empty_lines\([1-9][0-9]*\))$"
)
VERSION_RE = re.compile(r"^[0-9]+(?:\.[0-9]+)*$")
MAX_RULES_PER_MANIFEST = 128
MAX_GATE_DEPTH = 8
MAX_TOTAL_GATES = 512
MAX_MATCHERS_PER_GATE = 32
MAX_TOTAL_MATCHERS = 1024
MAX_MATCHER_CHARS = 512


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundled-dir", type=Path, default=DEFAULT_BUNDLED_DIR)
    parser.add_argument("--website-dir", type=Path, default=DEFAULT_WEBSITE_DIR)
    parser.add_argument("--engine-version", type=int)
    parser.add_argument(
        "--require-website",
        action="store_true",
        help="fail if website agent-detection assets or catalog are missing",
    )
    return parser.parse_args()


def read_engine_version(explicit: int | None) -> int:
    if explicit is not None:
        return explicit
    content = ENGINE_SOURCE.read_text(encoding="utf-8")
    match = re.search(r"MANIFEST_ENGINE_VERSION:\s*u32\s*=\s*([0-9]+)", content)
    if not match:
        raise CheckError(f"could not find MANIFEST_ENGINE_VERSION in {ENGINE_SOURCE}")
    return int(match.group(1))


class CheckError(Exception):
    pass


def load_toml(path: Path) -> dict:
    try:
        with path.open("rb") as fh:
            value = tomllib.load(fh)
    except tomllib.TOMLDecodeError as exc:
        raise CheckError(f"{path}: invalid TOML: {exc}") from exc
    if not isinstance(value, dict):
        raise CheckError(f"{path}: TOML root must be a table")
    return value


def version_tuple(value: str, path: Path) -> tuple[int, ...]:
    if not isinstance(value, str) or not VERSION_RE.fullmatch(value):
        raise CheckError(f"{path}: version must be dotted numeric")
    return tuple(int(part) for part in value.split("."))


def compare_versions(left: str, right: str, path: Path) -> int:
    left_parts = list(version_tuple(left, path))
    right_parts = list(version_tuple(right, path))
    width = max(len(left_parts), len(right_parts))
    left_parts.extend([0] * (width - len(left_parts)))
    right_parts.extend([0] * (width - len(right_parts)))
    return (left_parts > right_parts) - (left_parts < right_parts)


def validate_manifest(path: Path, engine_version: int) -> dict:
    manifest = load_toml(path)
    unknown = sorted(set(manifest) - MANIFEST_KEYS)
    if unknown:
        raise CheckError(f"{path}: unknown manifest field(s): {', '.join(unknown)}")

    agent_id = manifest.get("id")
    if not isinstance(agent_id, str) or not agent_id.strip():
        raise CheckError(f"{path}: id must be a non-empty string")

    version = manifest.get("version")
    version_tuple(version, path)

    min_engine = manifest.get("min_engine_version")
    if not isinstance(min_engine, int):
        raise CheckError(f"{path}: min_engine_version must be an integer")
    if min_engine > engine_version:
        raise CheckError(
            f"{path}: min_engine_version {min_engine} exceeds engine {engine_version}"
        )

    aliases = manifest.get("aliases", [])
    if not isinstance(aliases, list) or not all(isinstance(item, str) for item in aliases):
        raise CheckError(f"{path}: aliases must be an array of strings")

    rules = manifest.get("rules")
    if not isinstance(rules, list) or not rules:
        raise CheckError(f"{path}: rules must be a non-empty array")
    if len(rules) > MAX_RULES_PER_MANIFEST:
        raise CheckError(f"{path}: manifest exceeds max rule count {MAX_RULES_PER_MANIFEST}")
    complexity = {"gates": 0, "matchers": 0}
    for index, rule in enumerate(rules):
        validate_rule(path, index, rule, complexity)

    return manifest


def validate_rule(path: Path, index: int, rule: object, complexity: dict[str, int]) -> None:
    if not isinstance(rule, dict):
        raise CheckError(f"{path}: rule {index} must be a table")
    unknown = sorted(set(rule) - RULE_KEYS)
    if unknown:
        raise CheckError(f"{path}: rule {index} has unknown field(s): {', '.join(unknown)}")
    rule_id = rule.get("id")
    if not isinstance(rule_id, str) or not rule_id.strip():
        raise CheckError(f"{path}: rule {index} id must be a non-empty string")
    state = rule.get("state")
    if state is not None and state not in STATES:
        raise CheckError(f"{path}: rule {rule_id} has invalid state {state!r}")
    region = rule.get("region", "whole_recent")
    if not isinstance(region, str) or not REGION_RE.fullmatch(region):
        raise CheckError(f"{path}: rule {rule_id} has invalid region {region!r}")
    if rule.get("skip_state_update"):
        if state != "unknown":
            raise CheckError(f"{path}: rule {rule_id} skip_state_update requires state unknown")
        if rule.get("visible_idle") or rule.get("visible_blocker") or rule.get("visible_working"):
            raise CheckError(f"{path}: rule {rule_id} skip_state_update cannot set visible flags")
    validate_gate(path, f"rule {rule_id}", rule, require_positive=True, depth=0, complexity=complexity)


def validate_gate(
    path: Path,
    label: str,
    gate: dict,
    require_positive: bool,
    depth: int,
    complexity: dict[str, int],
) -> None:
    if depth > MAX_GATE_DEPTH:
        raise CheckError(f"{path}: {label} exceeds max gate depth {MAX_GATE_DEPTH}")
    complexity["gates"] += 1
    if complexity["gates"] > MAX_TOTAL_GATES:
        raise CheckError(f"{path}: manifest exceeds max gate count {MAX_TOTAL_GATES}")
    unknown = sorted(set(gate) - (RULE_KEYS if label.startswith("rule ") else GATE_KEYS))
    if unknown:
        raise CheckError(f"{path}: {label} has unknown gate field(s): {', '.join(unknown)}")
    matcher_count = 0
    for key in ("contains", "regex", "line_regex"):
        values = gate.get(key, [])
        if not isinstance(values, list) or not all(isinstance(item, str) for item in values):
            raise CheckError(f"{path}: {label} {key} must be an array of strings")
        matcher_count += len(values)
        for value in values:
            if len(value) > MAX_MATCHER_CHARS:
                raise CheckError(f"{path}: {label} matcher exceeds max length {MAX_MATCHER_CHARS}")
    if matcher_count > MAX_MATCHERS_PER_GATE:
        raise CheckError(f"{path}: {label} exceeds max direct matcher count {MAX_MATCHERS_PER_GATE}")
    complexity["matchers"] += matcher_count
    if complexity["matchers"] > MAX_TOTAL_MATCHERS:
        raise CheckError(f"{path}: manifest exceeds max matcher count {MAX_TOTAL_MATCHERS}")
    nested_any = gate.get("any", [])
    nested_all = gate.get("all", [])
    nested_not = gate.get("not", [])
    for key, values in (("any", nested_any), ("all", nested_all), ("not", nested_not)):
        if not isinstance(values, list):
            raise CheckError(f"{path}: {label} {key} must be an array")
    if require_positive and not has_positive_matcher(gate):
        raise CheckError(f"{path}: {label} must contain a positive matcher")
    for idx, nested in enumerate(nested_any):
        validate_nested_gate(path, f"{label} any[{idx}]", nested, require_positive=True, depth=depth + 1, complexity=complexity)
    for idx, nested in enumerate(nested_all):
        validate_nested_gate(path, f"{label} all[{idx}]", nested, require_positive=True, depth=depth + 1, complexity=complexity)
    for idx, nested in enumerate(nested_not):
        validate_nested_gate(path, f"{label} not[{idx}]", nested, require_positive=False, depth=depth + 1, complexity=complexity)


def validate_nested_gate(
    path: Path,
    label: str,
    gate: object,
    require_positive: bool,
    depth: int,
    complexity: dict[str, int],
) -> None:
    if not isinstance(gate, dict):
        raise CheckError(f"{path}: {label} must be a table")
    if not require_positive and not has_any_matcher(gate):
        raise CheckError(f"{path}: {label} must contain a matcher")
    validate_gate(path, label, gate, require_positive=require_positive, depth=depth, complexity=complexity)


def has_positive_matcher(gate: dict) -> bool:
    return bool(gate.get("contains") or gate.get("regex") or gate.get("line_regex") or gate.get("any") or gate.get("all"))


def has_any_matcher(gate: dict) -> bool:
    return bool(
        gate.get("contains")
        or gate.get("regex")
        or gate.get("line_regex")
        or gate.get("any")
        or gate.get("all")
        or gate.get("not")
    )


def load_manifest_dir(path: Path, engine_version: int) -> dict[str, tuple[Path, dict]]:
    if not path.is_dir():
        raise CheckError(f"{path}: manifest directory is missing")
    manifests: dict[str, tuple[Path, dict]] = {}
    for manifest_path in sorted(path.glob("*.toml")):
        if manifest_path.name == "index.toml":
            continue
        manifest = validate_manifest(manifest_path, engine_version)
        agent_id = manifest["id"]
        if agent_id in manifests:
            raise CheckError(
                f"{manifest_path}: duplicate manifest id {agent_id!r}; already seen in {manifests[agent_id][0]}"
            )
        manifests[agent_id] = (manifest_path, manifest)
    if not manifests:
        raise CheckError(f"{path}: no manifests found")
    return manifests


def validate_catalog(
    website_dir: Path,
    bundled: dict[str, tuple[Path, dict]],
    engine_version: int,
) -> None:
    catalog_path = website_dir / "index.toml"
    catalog = load_toml(catalog_path)
    if set(catalog) != {"schema_version", "agents"}:
        raise CheckError(f"{catalog_path}: expected only schema_version and agents")
    if catalog.get("schema_version") != 1:
        raise CheckError(f"{catalog_path}: schema_version must be 1")
    agents = catalog.get("agents")
    if not isinstance(agents, list):
        raise CheckError(f"{catalog_path}: agents must be an array")

    seen: dict[str, str] = {}
    for entry in agents:
        if not isinstance(entry, dict) or set(entry) != {"id", "path"}:
            raise CheckError(f"{catalog_path}: each agent entry must contain id and path")
        agent_id = entry["id"]
        rel_path = entry["path"]
        if not isinstance(agent_id, str) or not isinstance(rel_path, str):
            raise CheckError(f"{catalog_path}: agent id and path must be strings")
        if agent_id in seen:
            raise CheckError(f"{catalog_path}: duplicate catalog agent {agent_id}")
        if "://" in rel_path or rel_path.startswith("/") or ".." in Path(rel_path).parts:
            raise CheckError(f"{catalog_path}: unsafe path for {agent_id}: {rel_path}")
        if agent_id not in bundled:
            raise CheckError(f"{catalog_path}: unknown agent {agent_id}; binary cannot identify it")
        manifest_path = website_dir / rel_path
        manifest = validate_manifest(manifest_path, engine_version)
        if manifest["id"] != agent_id:
            raise CheckError(f"{manifest_path}: id {manifest['id']} does not match catalog {agent_id}")
        seen[agent_id] = rel_path

        bundled_path, bundled_manifest = bundled[agent_id]
        cmp = compare_versions(manifest["version"], bundled_manifest["version"], manifest_path)
        if cmp < 0:
            raise CheckError(
                f"{manifest_path}: website version {manifest['version']} is lower than bundled "
                f"{bundled_manifest['version']} in {bundled_path}"
            )
        if cmp == 0 and manifest_path.read_text(encoding="utf-8") != bundled_path.read_text(encoding="utf-8"):
            raise CheckError(
                f"{manifest_path}: same version as bundled {bundled_manifest['version']} but content differs"
            )

    missing = sorted(set(bundled) - set(seen))
    if missing:
        raise CheckError(f"{catalog_path}: missing bundled agent(s): {', '.join(missing)}")

    catalog_paths = set(seen.values()) | {"index.toml"}
    extra = sorted(path.name for path in website_dir.glob("*.toml") if path.name not in catalog_paths)
    if extra:
        raise CheckError(f"{website_dir}: TOML file(s) not listed in catalog: {', '.join(extra)}")


def main() -> int:
    args = parse_args()
    try:
        engine_version = read_engine_version(args.engine_version)
        bundled = load_manifest_dir(args.bundled_dir, engine_version)
        if args.require_website or args.website_dir.exists():
            if not args.website_dir.is_dir():
                raise CheckError(f"{args.website_dir}: website manifest directory is missing")
            validate_catalog(args.website_dir, bundled, engine_version)
    except CheckError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    print("agent detection manifests ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
