#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from datetime import date
from pathlib import Path
from typing import Any

SECTION_RE = re.compile(r"^##\s+(?:\[(?P<bracketed>[^\]]+)\]|(?P<plain>.+?))\s*$", re.MULTILINE)
VERSION_WITH_DATE_RE = re.compile(r"^(?P<version>.+?)\s+-\s+\d{4}-\d{2}-\d{2}$")
DEFAULT_RELEASE_REPO = "ogulcancelik/herdr"
DEFAULT_LATEST_JSON_PATH = Path("website/latest.json")
ASSET_TARGETS = (
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
)
EXPECTED_ASSET_NAMES = {target: f"herdr-{target}" for target in ASSET_TARGETS}


@dataclass(frozen=True)
class Section:
    title: str
    start: int
    end: int
    body_start: int


class ChangelogError(ValueError):
    pass


def normalize_title(raw_title: str) -> str:
    title = raw_title.strip()
    match = VERSION_WITH_DATE_RE.match(title)
    if match:
        title = match.group("version").strip()
    if title.startswith("[") and title.endswith("]"):
        title = title[1:-1].strip()
    return title


def normalize_version(version: str) -> str:
    return version.strip().removeprefix("v")


def parse_version(version: str) -> tuple[int, int, int]:
    normalized = normalize_version(version)
    parts = normalized.split(".")
    if len(parts) != 3:
        raise ChangelogError(f"invalid version: {version}")
    try:
        return tuple(int(part) for part in parts)  # type: ignore[return-value]
    except ValueError as exc:
        raise ChangelogError(f"invalid version: {version}") from exc


def parse_sections(text: str) -> list[Section]:
    matches = list(SECTION_RE.finditer(text))
    sections: list[Section] = []

    for index, match in enumerate(matches):
        title = normalize_title(match.group("bracketed") or match.group("plain") or "")
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        body_start = match.end()
        if body_start < len(text) and text[body_start : body_start + 1] == "\n":
            body_start += 1
        sections.append(Section(title=title, start=match.start(), end=end, body_start=body_start))

    return sections


def find_section(text: str, wanted_title: str) -> Section:
    for section in parse_sections(text):
        if section.title == wanted_title:
            return section
    raise ChangelogError(f"section not found: {wanted_title}")


def extract_section_body(text: str, wanted_title: str) -> str:
    section = find_section(text, wanted_title)
    body = text[section.body_start : section.end].strip("\n")
    if not body.strip():
        raise ChangelogError(f"section is empty: {wanted_title}")
    return body + "\n"


def prepare_release(text: str, version: str, release_date: str) -> str:
    unreleased = None
    existing_version = False

    for section in parse_sections(text):
        if section.title == "Unreleased":
            unreleased = section
        if section.title == version:
            existing_version = True

    if existing_version:
        raise ChangelogError(f"version already exists in changelog: {version}")
    if unreleased is None:
        raise ChangelogError("missing Unreleased section")

    unreleased_body = text[unreleased.body_start : unreleased.end].strip("\n")
    if not unreleased_body.strip():
        raise ChangelogError("Unreleased section is empty")

    prefix = text[: unreleased.start].rstrip("\n")
    suffix = text[unreleased.end :].strip("\n")

    rebuilt = f"## Unreleased\n\n## [{version}] - {release_date}\n\n{unreleased_body}"
    if suffix:
        rebuilt += f"\n\n{suffix}"

    if prefix:
        return f"{prefix}\n\n{rebuilt}\n"
    return rebuilt + "\n"


def build_latest_json(version: str, notes: str, assets: dict[str, str]) -> str:
    normalized_version = normalize_version(version)
    normalized_notes = notes.strip()
    if not normalized_notes:
        raise ChangelogError("release notes are empty")

    missing_targets = [target for target in ASSET_TARGETS if target not in assets]
    if missing_targets:
        raise ChangelogError(f"missing asset targets: {', '.join(missing_targets)}")

    ordered_assets = {target: assets[target] for target in ASSET_TARGETS}

    return json.dumps(
        {
            "version": normalized_version,
            "notes": normalized_notes,
            "assets": ordered_assets,
        },
        indent=2,
    ) + "\n"


def default_release_assets(version: str, repo: str = DEFAULT_RELEASE_REPO) -> dict[str, str]:
    normalized_version = normalize_version(version)
    tag = f"v{normalized_version}"
    return {
        target: f"https://github.com/{repo}/releases/download/{tag}/{EXPECTED_ASSET_NAMES[target]}"
        for target in ASSET_TARGETS
    }


def manifest_from_release_payload(payload: dict[str, Any], version: str) -> dict[str, Any]:
    normalized_version = normalize_version(version)
    tag_name = str(payload.get("tagName") or "")
    if normalize_version(tag_name) != normalized_version:
        raise ChangelogError(
            f"GitHub release tag mismatch: expected v{normalized_version}, got {tag_name or '<missing>'}"
        )
    if payload.get("isDraft"):
        raise ChangelogError(f"GitHub release v{normalized_version} is still a draft")
    if payload.get("isPrerelease"):
        raise ChangelogError(f"GitHub release v{normalized_version} is a prerelease")

    notes = str(payload.get("body") or "").strip()
    if not notes:
        raise ChangelogError(f"GitHub release v{normalized_version} has empty release notes")

    assets_list = payload.get("assets")
    if not isinstance(assets_list, list):
        raise ChangelogError("GitHub release response is missing assets")

    release_assets: dict[str, Any] = {}
    for asset in assets_list:
        if isinstance(asset, dict):
            name = asset.get("name")
            if isinstance(name, str) and name not in release_assets:
                release_assets[name] = asset

    manifest_assets: dict[str, str] = {}
    for target, asset_name in EXPECTED_ASSET_NAMES.items():
        asset = release_assets.get(asset_name)
        if not isinstance(asset, dict):
            raise ChangelogError(f"GitHub release v{normalized_version} is missing asset {asset_name}")
        url = str(asset.get("url") or "").strip()
        if not url:
            raise ChangelogError(f"GitHub release asset {asset_name} is missing a download URL")
        manifest_assets[target] = url

    return {
        "version": normalized_version,
        "notes": notes,
        "assets": manifest_assets,
    }


def load_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise ChangelogError(f"file not found: {path}") from exc


def load_json(path: Path) -> dict[str, Any]:
    try:
        content = path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise ChangelogError(f"file not found: {path}") from exc

    try:
        data = json.loads(content)
    except json.JSONDecodeError as exc:
        raise ChangelogError(f"invalid JSON in {path}: {exc}") from exc

    if not isinstance(data, dict):
        raise ChangelogError(f"expected JSON object in {path}")
    return data


def write_text(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def fetch_release_payload(version: str, repo: str) -> dict[str, Any]:
    normalized_version = normalize_version(version)
    command = [
        "gh",
        "release",
        "view",
        f"v{normalized_version}",
        "--repo",
        repo,
        "--json",
        "tagName,isDraft,isPrerelease,body,assets",
    ]
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        stderr = result.stderr.strip() or result.stdout.strip() or "unknown gh error"
        raise ChangelogError(f"failed to read GitHub release v{normalized_version}: {stderr}")

    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise ChangelogError(f"invalid JSON from gh release view: {exc}") from exc

    if not isinstance(payload, dict):
        raise ChangelogError("unexpected GitHub release payload shape")
    return payload


def ensure_manifest_is_outdated(current_manifest: dict[str, Any], version: str) -> None:
    current_version = current_manifest.get("version")
    if not isinstance(current_version, str):
        raise ChangelogError("website/latest.json is missing a string version")

    if parse_version(current_version) >= parse_version(version):
        raise ChangelogError(
            f"website/latest.json is already at v{normalize_version(current_version)}; expected something older than v{normalize_version(version)}"
        )


def git_status_lines(path: Path) -> list[str]:
    result = subprocess.run(
        ["git", "status", "--short", "--", str(path)],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return []
    return [line for line in result.stdout.splitlines() if line.strip()]


def cmd_prepare(args: argparse.Namespace) -> int:
    path = Path(args.path)
    original = load_text(path)
    updated = prepare_release(original, normalize_version(args.version), args.date)
    write_text(path, updated)
    return 0


def cmd_extract(args: argparse.Namespace) -> int:
    path = Path(args.path)
    body = extract_section_body(load_text(path), normalize_version(args.version))
    if args.output:
        write_text(Path(args.output), body)
    else:
        sys.stdout.write(body)
    return 0


def cmd_sync_latest_json(args: argparse.Namespace) -> int:
    manifest_path = Path(args.output)
    version = normalize_version(args.version)

    current_manifest = load_json(manifest_path)
    ensure_manifest_is_outdated(current_manifest, version)

    release_payload = fetch_release_payload(version, args.repo)
    new_manifest = manifest_from_release_payload(release_payload, version)
    output = build_latest_json(version, str(new_manifest["notes"]), dict(new_manifest["assets"]))
    write_text(manifest_path, output)

    print(f"updated {manifest_path} from GitHub release v{version}")
    status_lines = git_status_lines(manifest_path)
    print("files changed:")
    if status_lines:
        for line in status_lines:
            print(f"  {line}")
    else:
        print(f"  (no git status output for {manifest_path})")

    print("next:")
    print(f"  git diff -- {manifest_path}")
    print(f"  git add {manifest_path}")
    print(f"  git commit -m \"docs: update website manifest for v{version}\"")
    print("  git push")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Prepare and extract changelog release notes")
    subparsers = parser.add_subparsers(dest="command", required=True)

    prepare = subparsers.add_parser("prepare", help="Move Unreleased into a versioned section")
    prepare.add_argument("--path", default="CHANGELOG.md")
    prepare.add_argument("--version", required=True)
    prepare.add_argument("--date", default=str(date.today()))
    prepare.set_defaults(func=cmd_prepare)

    extract = subparsers.add_parser("extract", help="Extract a version section body")
    extract.add_argument("--path", default="CHANGELOG.md")
    extract.add_argument("--version", required=True)
    extract.add_argument("--output")
    extract.set_defaults(func=cmd_extract)

    sync_latest_json = subparsers.add_parser(
        "sync-latest-json",
        help="Update website/latest.json from a published GitHub release",
    )
    sync_latest_json.add_argument("--version", required=True)
    sync_latest_json.add_argument("--repo", default=DEFAULT_RELEASE_REPO)
    sync_latest_json.add_argument("--output", default=str(DEFAULT_LATEST_JSON_PATH))
    sync_latest_json.set_defaults(func=cmd_sync_latest_json)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    try:
        return args.func(args)
    except ChangelogError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
