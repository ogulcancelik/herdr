"""Check the website config reference against the Rust config model.

Walks the serde structs in src/config/*.rs starting from the root `Config`
struct, builds the set of canonical dotted TOML key paths, and compares it
against the keys documented in the preview config reference. Compatibility
aliases are intentionally documented on their canonical entries rather than
as separate rows.

The comparison checks key names and serde-derived enum values, so failures
name exact missing, stale, duplicated, or value-drifted entries.

Open-ended surfaces (arrays of tables such as [[keys.command]]) are not
enumerable per-key and are skipped; they are listed in SKIPPED_SUBTREES so
the skip stays explicit.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path


DEFAULT_MODEL_ROOT = Path("src/config")
DEFAULT_REFERENCE = Path("docs/next/website/src/data/config-reference.json")
ROOT_STRUCT = "Config"

# Dotted key prefixes that are open-ended (user-defined tables/arrays) and
# therefore not enumerable in a flat reference table.
SKIPPED_SUBTREES = ("keys.command",)

FIELD_RE = re.compile(r"^\s*pub ([a-z_][a-z0-9_]*):\s*(.+?),?\s*$")
STRUCT_RE = re.compile(r"^\s*pub(?:\(crate\))? struct ([A-Za-z0-9_]+)\s*\{\s*$")
ENUM_RE = re.compile(r"^\s*pub(?:\(crate\))? enum ([A-Za-z0-9_]+)\s*\{\s*$")
VARIANT_RE = re.compile(r"^\s*([A-Z][A-Za-z0-9_]*)\s*(?:\(.*\))?\s*,?\s*$")
RENAME_ALL_RE = re.compile(r'rename_all\s*=\s*"([^"]+)"')
RENAME_RE = re.compile(r'rename\s*=\s*"([^"]+)"')


@dataclass
class StructField:
    name: str
    rust_type: str
    doc: str


@dataclass
class Model:
    structs: dict[str, list[StructField]] = field(default_factory=dict)
    enums: dict[str, list[str]] = field(default_factory=dict)


def apply_rename_all(name: str, style: str | None) -> str:
    if style is None:
        return name
    if style == "lowercase":
        return name.lower()
    if style == "snake_case":
        return re.sub(r"(?<!^)([A-Z])", r"_\1", name).lower()
    if style == "kebab-case":
        return re.sub(r"(?<!^)([A-Z])", r"-\1", name).lower()
    raise ValueError(f"unsupported serde rename_all style: {style}")


def parse_model(paths: list[Path]) -> Model:
    model = Model()
    for path in paths:
        parse_file(path.read_text(encoding="utf-8"), model)
    return model


def parse_file(text: str, model: Model) -> None:
    lines = text.splitlines()
    index = 0
    pending_attrs: list[str] = []

    while index < len(lines):
        line = lines[index]
        stripped = line.strip()

        if stripped.startswith("#["):
            pending_attrs.append(stripped)
            index += 1
            continue

        struct_match = STRUCT_RE.match(line)
        enum_match = ENUM_RE.match(line)
        if struct_match:
            rename_all = find_rename_all(pending_attrs)
            index = parse_struct_body(lines, index + 1, struct_match.group(1), rename_all, model)
            pending_attrs = []
            continue
        if enum_match:
            rename_all = find_rename_all(pending_attrs)
            untagged = any("untagged" in attr for attr in pending_attrs if attr.startswith("#[serde"))
            index = parse_enum_body(lines, index + 1, enum_match.group(1), rename_all, model)
            if untagged:
                model.enums.pop(enum_match.group(1), None)
            pending_attrs = []
            continue

        if not stripped.startswith("///"):
            pending_attrs = []
        index += 1


def find_rename_all(attrs: list[str]) -> str | None:
    for attr in attrs:
        if not attr.startswith("#[serde"):
            continue
        match = RENAME_ALL_RE.search(attr)
        if match:
            return match.group(1)
    return None


def parse_struct_body(
    lines: list[str], start: int, name: str, rename_all: str | None, model: Model
) -> int:
    fields: list[StructField] = []
    doc_lines: list[str] = []
    field_attrs: list[str] = []
    index = start

    while index < len(lines):
        stripped = lines[index].strip()
        if stripped == "}":
            index += 1
            break

        if stripped.startswith("///"):
            doc_lines.append(stripped.lstrip("/").strip())
            index += 1
            continue
        if stripped.startswith("#["):
            field_attrs.append(stripped)
            index += 1
            continue

        match = FIELD_RE.match(lines[index])
        if match:
            field_name = serde_field_name(match.group(1), field_attrs, rename_all)
            if not is_skipped_field(field_attrs):
                fields.append(
                    StructField(
                        name=field_name,
                        rust_type=match.group(2).strip(),
                        doc=" ".join(doc_lines),
                    )
                )
        doc_lines = []
        field_attrs = []
        index += 1

    model.structs[name] = fields
    return index


def serde_field_name(name: str, attrs: list[str], rename_all: str | None) -> str:
    for attr in attrs:
        if attr.startswith("#[serde"):
            match = RENAME_RE.search(attr)
            if match and "rename_all" not in attr:
                return match.group(1)
    return apply_rename_all(name, rename_all) if rename_all else name


def is_skipped_field(attrs: list[str]) -> bool:
    for attr in attrs:
        if not attr.startswith("#[serde"):
            continue
        if re.search(r"\bskip\b|\bskip_deserializing\b", attr):
            return True
    return False


def parse_enum_body(
    lines: list[str], start: int, name: str, rename_all: str | None, model: Model
) -> int:
    variants: list[str] = []
    index = start
    depth = 0

    while index < len(lines):
        stripped = lines[index].strip()
        if depth == 0 and stripped == "}":
            index += 1
            break

        depth += stripped.count("{") - stripped.count("}")
        if depth == 0 and not stripped.startswith(("#[", "///")):
            match = VARIANT_RE.match(stripped)
            if match:
                variants.append(apply_rename_all(match.group(1), rename_all or "lowercase"))
        index += 1

    model.enums[name] = variants
    return index


def strip_wrappers(rust_type: str) -> tuple[str, bool]:
    """Return the innermost type name and whether it was wrapped in Vec."""
    current = rust_type
    is_vec = False
    while True:
        match = re.fullmatch(r"(Option|Vec|Box)<(.+)>", current)
        if not match:
            break
        if match.group(1) == "Vec":
            is_vec = True
        current = match.group(2)
    # std::collections::BTreeSet<...> and friends stay as-is; they are leaves.
    return current.strip(), is_vec


def collect_keys(model: Model) -> set[str]:
    return {entry["key"] for entry in collect_entries(model)}


def collect_entries(model: Model, struct_name: str = ROOT_STRUCT, prefix: str = "") -> list[dict]:
    """Dotted keys with type/doc details; --emit prints these for bootstrapping."""
    if struct_name not in model.structs:
        raise KeyError(f"struct {struct_name} not found in config model")

    entries: list[dict] = []
    for struct_field in model.structs[struct_name]:
        dotted = f"{prefix}{struct_field.name}"
        inner, is_vec = strip_wrappers(struct_field.rust_type)
        if inner in model.structs:
            if dotted in SKIPPED_SUBTREES:
                continue
            if is_vec:
                raise ValueError(
                    f"{dotted} is an open-ended array of tables; add it to "
                    "SKIPPED_SUBTREES and document it in prose"
                )
            entries.extend(collect_entries(model, inner, f"{dotted}."))
        else:
            entry = {"key": dotted, "rust_type": struct_field.rust_type, "doc": struct_field.doc}
            if inner in model.enums:
                entry["values"] = model.enums[inner]
            entries.append(entry)
    return entries


def reference_entries(reference_path: Path) -> tuple[dict[str, dict], list[str]]:
    data = json.loads(reference_path.read_text(encoding="utf-8"))
    entries: dict[str, dict] = {}
    errors: list[str] = []
    for section in data["sections"]:
        for entry in section["keys"]:
            key = entry["key"]
            if key in entries:
                errors.append(f"{key}: duplicated in {reference_path}")
                continue
            entries[key] = entry
    return entries, errors


def check(model_root: Path, reference_path: Path) -> list[str]:
    model = parse_model(sorted(model_root.glob("*.rs")))
    code_entries = {entry["key"]: entry for entry in collect_entries(model)}
    doc_entries, errors = reference_entries(reference_path)
    code_keys = set(code_entries)
    doc_keys = set(doc_entries)

    for missing in sorted(code_keys - doc_keys):
        errors.append(f"{missing}: in src/config but missing from {reference_path}")
    for stale in sorted(doc_keys - code_keys):
        errors.append(f"{stale}: in {reference_path} but not in src/config")
    for key in sorted(code_keys & doc_keys):
        expected_values = code_entries[key].get("values")
        if expected_values is None:
            continue
        documented_values = doc_entries[key].get("values")
        if documented_values != expected_values:
            errors.append(
                f"{key}: allowed values in {reference_path} are "
                f"{documented_values!r}; expected {expected_values!r} from src/config"
            )
    return errors


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Check website config reference keys against src/config structs."
    )
    parser.add_argument("--model-root", default=DEFAULT_MODEL_ROOT, type=Path)
    parser.add_argument("--reference", default=DEFAULT_REFERENCE, type=Path)
    parser.add_argument(
        "--emit",
        action="store_true",
        help="Print extracted keys with types and doc comments as JSON and exit.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)

    if args.emit:
        model = parse_model(sorted(args.model_root.glob("*.rs")))
        print(json.dumps(collect_entries(model), indent=2))
        return 0

    errors = check(args.model_root, args.reference)
    if errors:
        print("error: config reference is out of sync with src/config", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
