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

DEFAULT_LIVE_MANIFEST_URL = "https://herdr.dev/latest.json"

SECTION_RE = re.compile(r"^##\s+(?:\[(?P<bracketed>[^\]]+)\]|(?P<plain>.+?))\s*$", re.MULTILINE)
VERSION_WITH_DATE_RE = re.compile(r"^(?P<version>.+?)\s+-\s+\d{4}-\d{2}-\d{2}$")
DEFAULT_RELEASE_REPO = "ogulcancelik/herdr"
DEFAULT_LATEST_JSON_PATH = Path("website/latest.json")
DEFAULT_PRODUCT_ANNOUNCEMENT_PATH = Path("docs/next/product-announcement.json")
PROTOCOL_SOURCE_PATH = Path("src/protocol/wire.rs")
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


def read_protocol_version(source_path: Path = PROTOCOL_SOURCE_PATH) -> int:
    content = source_path.read_text(encoding="utf-8")
    match = re.search(r"pub const PROTOCOL_VERSION: u32 = (\d+);", content)
    if not match:
        raise ChangelogError(f"could not read PROTOCOL_VERSION from {source_path}")
    return int(match.group(1))


def normalize_announcement(value: Any, label: str) -> dict[str, str] | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        raise ChangelogError(f"{label} announcement must be an object")

    allowed_keys = {"id", "title", "body"}
    extra_keys = sorted(set(value) - allowed_keys)
    if extra_keys:
        raise ChangelogError(
            f"{label} announcement has unsupported field(s): {', '.join(extra_keys)}"
        )

    announcement: dict[str, str] = {}
    for key in ("id", "title", "body"):
        field_value = value.get(key)
        if not isinstance(field_value, str) or not field_value.strip():
            raise ChangelogError(f"{label} announcement is missing non-empty string field: {key}")
        announcement[key] = field_value.strip()

    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]*", announcement["id"]):
        raise ChangelogError(
            f"{label} announcement has invalid id; use lowercase letters, numbers, dots, underscores, or dashes"
        )

    return announcement


def infer_protocol_from_notes(notes: str) -> int | None:
    match = re.search(r"protocol(?: is now)? version (\d+)", notes, flags=re.IGNORECASE)
    if match is None:
        return None
    return int(match.group(1))


def normalize_assets(value: Any, label: str) -> dict[str, str]:
    if not isinstance(value, dict):
        raise ChangelogError(f"{label} must be an object")

    missing_targets = [target for target in ASSET_TARGETS if target not in value]
    if missing_targets:
        raise ChangelogError(f"{label} is missing asset URL for {', '.join(missing_targets)}")

    normalized_assets: dict[str, str] = {}
    for target in ASSET_TARGETS:
        url = value.get(target)
        if not isinstance(url, str) or not url.strip():
            raise ChangelogError(f"{label} is missing asset URL for {target}")
        normalized_assets[target] = url.strip()
    return normalized_assets


def normalize_release_metadata(value: Any, label: str, version: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ChangelogError(f"{label} must be an object")

    allowed_keys = {"notes", "announcement", "assets", "protocol"}
    extra_keys = sorted(set(value) - allowed_keys)
    if extra_keys:
        raise ChangelogError(f"{label} has unsupported field(s): {', '.join(extra_keys)}")

    notes = value.get("notes")
    if not isinstance(notes, str) or not notes.strip():
        raise ChangelogError(f"{label} is missing non-empty release notes")

    metadata: dict[str, Any] = {"notes": notes.strip()}
    protocol = value.get("protocol")
    if protocol is not None:
        if not isinstance(protocol, int):
            raise ChangelogError(f"{label}.protocol must be an integer")
        metadata["protocol"] = protocol
    else:
        inferred_protocol = infer_protocol_from_notes(notes)
        if inferred_protocol is not None:
            metadata["protocol"] = inferred_protocol
    if "assets" in value:
        metadata["assets"] = normalize_assets(value.get("assets"), f"{label}.assets")
    else:
        metadata["assets"] = default_release_assets(version)
    announcement = normalize_announcement(value.get("announcement"), label)
    if announcement is not None:
        metadata["announcement"] = announcement
    return metadata


def normalize_releases(value: Any) -> dict[str, dict[str, Any]]:
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise ChangelogError("releases must be an object")

    releases: dict[str, dict[str, Any]] = {}
    for raw_version, raw_metadata in value.items():
        if not isinstance(raw_version, str) or not raw_version.strip():
            raise ChangelogError("releases contains an empty version key")
        version = normalize_version(raw_version)
        parse_version(version)
        releases[version] = normalize_release_metadata(raw_metadata, f"releases.{version}", version)

    return {
        version: releases[version]
        for version in sorted(releases, key=parse_version, reverse=True)
    }


def build_latest_json(
    version: str,
    notes: str,
    assets: dict[str, str],
    protocol: int | None = None,
    announcement: dict[str, str] | None = None,
    releases: dict[str, Any] | None = None,
) -> str:
    normalized_version = normalize_version(version)
    normalized_notes = notes.strip()
    if not normalized_notes:
        raise ChangelogError("release notes are empty")

    if protocol is None:
        protocol = read_protocol_version()

    ordered_assets = normalize_assets(assets, "assets")
    normalized_announcement = normalize_announcement(announcement, "root")
    archived_releases = normalize_releases(releases)
    current_metadata: dict[str, Any] = {
        "notes": normalized_notes,
        "protocol": protocol,
        "assets": ordered_assets,
    }
    if normalized_announcement is not None:
        current_metadata["announcement"] = normalized_announcement
    archived_releases[normalized_version] = current_metadata
    archived_releases = {
        release_version: archived_releases[release_version]
        for release_version in sorted(archived_releases, key=parse_version, reverse=True)
    }

    manifest: dict[str, Any] = {
        "version": normalized_version,
        "protocol": protocol,
        "notes": normalized_notes,
        "assets": ordered_assets,
    }
    if normalized_announcement is not None:
        manifest["announcement"] = normalized_announcement
    manifest["releases"] = archived_releases

    return json.dumps(manifest, indent=2) + "\n"


def default_release_assets(version: str, repo: str = DEFAULT_RELEASE_REPO) -> dict[str, str]:
    normalized_version = normalize_version(version)
    tag = f"v{normalized_version}"
    return {
        target: f"https://github.com/{repo}/releases/download/{tag}/{EXPECTED_ASSET_NAMES[target]}"
        for target in ASSET_TARGETS
    }


def manifest_from_release_payload(
    payload: dict[str, Any], version: str, protocol: int | None = None
) -> dict[str, Any]:
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
        "protocol": protocol if protocol is not None else read_protocol_version(),
        "notes": notes,
        "assets": manifest_assets,
    }


def canonicalize_manifest(manifest: dict[str, Any], label: str) -> dict[str, Any]:
    version = manifest.get("version")
    if not isinstance(version, str) or not version.strip():
        raise ChangelogError(f"{label} is missing a string version")

    notes = manifest.get("notes")
    if not isinstance(notes, str) or not notes.strip():
        raise ChangelogError(f"{label} is missing non-empty release notes")

    protocol = manifest.get("protocol")
    if not isinstance(protocol, int):
        raise ChangelogError(f"{label} is missing an integer protocol")

    assets = manifest.get("assets")
    if not isinstance(assets, dict):
        raise ChangelogError(f"{label} is missing an assets object")

    normalized_assets = normalize_assets(assets, f"{label} assets")

    return {
        "version": normalize_version(version),
        "protocol": protocol,
        "notes": notes.strip(),
        "assets": normalized_assets,
    }


def ensure_manifest_matches_expected(
    manifest: dict[str, Any],
    expected_manifest: dict[str, Any],
    label: str,
) -> dict[str, Any]:
    canonical_manifest = canonicalize_manifest(manifest, label)
    canonical_expected = canonicalize_manifest(expected_manifest, "expected release manifest")
    if canonical_manifest != canonical_expected:
        raise ChangelogError(
            f"{label} does not match the published GitHub release manifest for v{canonical_expected['version']}"
        )
    return canonical_manifest


def ensure_current_release_assets_are_mirrored(manifest: dict[str, Any], label: str) -> None:
    canonical = canonicalize_manifest(manifest, label)
    releases = normalize_releases(manifest.get("releases"))
    metadata = releases.get(canonical["version"])
    if metadata is None:
        raise ChangelogError(f"{label} is missing releases.{canonical['version']}")
    if metadata.get("assets") != canonical["assets"]:
        raise ChangelogError(
            f"{label} releases.{canonical['version']}.assets must match top-level assets"
        )


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


def archived_releases_from_current_manifest(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    releases = normalize_releases(manifest.get("releases"))
    version = manifest.get("version")
    notes = manifest.get("notes")
    if isinstance(version, str) and version.strip() and isinstance(notes, str) and notes.strip():
        normalized_version = normalize_version(version)
        metadata: dict[str, Any] = {"notes": notes.strip()}
        protocol = manifest.get("protocol")
        if isinstance(protocol, int):
            metadata["protocol"] = protocol
        assets = manifest.get("assets")
        if isinstance(assets, dict):
            metadata["assets"] = normalize_assets(assets, "current root assets")
        else:
            metadata["assets"] = default_release_assets(normalized_version)
        announcement = normalize_announcement(manifest.get("announcement"), "current root")
        if announcement is not None:
            metadata["announcement"] = announcement
        releases[normalized_version] = metadata

    return {
        release_version: releases[release_version]
        for release_version in sorted(releases, key=parse_version, reverse=True)
    }


def load_product_announcement(path: Path) -> dict[str, str] | None:
    try:
        content = path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise ChangelogError(f"product announcement file not found: {path}") from exc

    try:
        data = json.loads(content)
    except json.JSONDecodeError as exc:
        raise ChangelogError(f"invalid JSON in {path}: {exc}") from exc

    if data is None:
        return None
    return normalize_announcement(data, f"announcement in {path}")


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


def fetch_remote_json(url: str, label: str) -> dict[str, Any]:
    command = [
        "curl",
        "-fsSL",
        "--retry",
        "3",
        "--connect-timeout",
        "10",
        "--max-time",
        "20",
        url,
    ]
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        stderr = result.stderr.strip() or result.stdout.strip() or "unknown curl error"
        raise ChangelogError(f"failed to fetch {label}: {stderr}")

    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise ChangelogError(f"invalid JSON from {label}: {exc}") from exc

    if not isinstance(payload, dict):
        raise ChangelogError(f"expected JSON object from {label}")
    return payload


def verify_asset_urls_resolve(assets: dict[str, str], label: str) -> None:
    for target in ASSET_TARGETS:
        url = assets[target]
        command = [
            "curl",
            "-fsSIL",
            "--retry",
            "3",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            url,
        ]
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        if result.returncode != 0:
            stderr = result.stderr.strip() or result.stdout.strip() or "unknown curl error"
            raise ChangelogError(f"failed to verify {label} asset {target}: {stderr}")


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
    new_manifest = manifest_from_release_payload(release_payload, version, args.protocol)
    announcement_path = Path(args.announcement)
    announcement = load_product_announcement(announcement_path)
    output = build_latest_json(
        version,
        str(new_manifest["notes"]),
        dict(new_manifest["assets"]),
        protocol=int(new_manifest["protocol"]),
        announcement=announcement,
        releases=archived_releases_from_current_manifest(current_manifest),
    )
    write_text(manifest_path, output)
    if announcement is not None:
        write_text(announcement_path, "null\n")

    print(f"updated {manifest_path} from GitHub release v{version}")
    if announcement is not None:
        print(f"included product announcement from {announcement_path}")
        print(f"cleared {announcement_path}")
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


def cmd_validate_product_announcement(args: argparse.Namespace) -> int:
    announcement = load_product_announcement(Path(args.path))
    if announcement is None:
        print(f"product announcement ({args.path}): none")
    else:
        print(
            f"product announcement ({args.path}): {announcement['id']} - {announcement['title']}"
        )
    return 0


def cmd_verify_release_state(args: argparse.Namespace) -> int:
    version = normalize_version(args.version)
    release_payload = fetch_release_payload(version, args.repo)
    expected_manifest = manifest_from_release_payload(release_payload, version, args.protocol)

    local_raw_manifest = load_json(Path(args.output))
    local_manifest = ensure_manifest_matches_expected(
        local_raw_manifest,
        expected_manifest,
        str(args.output),
    )
    ensure_current_release_assets_are_mirrored(local_raw_manifest, str(args.output))
    print(f"GitHub release v{version}: OK")
    print(f"local manifest ({args.output}): OK")

    live_raw_manifest = fetch_remote_json(args.live_url, args.live_url)
    live_manifest = ensure_manifest_matches_expected(
        live_raw_manifest,
        expected_manifest,
        args.live_url,
    )
    ensure_current_release_assets_are_mirrored(live_raw_manifest, args.live_url)
    print(f"live manifest ({args.live_url}): OK")

    verify_asset_urls_resolve(dict(expected_manifest["assets"]), "release")
    print("release asset URLs: OK")

    if local_manifest != live_manifest:
        raise ChangelogError("local and live manifests disagree after individual verification")
    print("local and live manifests agree: OK")
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
    sync_latest_json.add_argument("--announcement", default=str(DEFAULT_PRODUCT_ANNOUNCEMENT_PATH))
    sync_latest_json.add_argument("--protocol", type=int)
    sync_latest_json.set_defaults(func=cmd_sync_latest_json)

    validate_product_announcement = subparsers.add_parser(
        "validate-product-announcement",
        help="Validate docs/next product announcement JSON",
    )
    validate_product_announcement.add_argument(
        "--path", default=str(DEFAULT_PRODUCT_ANNOUNCEMENT_PATH)
    )
    validate_product_announcement.set_defaults(func=cmd_validate_product_announcement)

    verify_release_state = subparsers.add_parser(
        "verify-release-state",
        help="Verify GitHub release, local manifest, live manifest, and asset URLs all match",
    )
    verify_release_state.add_argument("--version", required=True)
    verify_release_state.add_argument("--repo", default=DEFAULT_RELEASE_REPO)
    verify_release_state.add_argument("--output", default=str(DEFAULT_LATEST_JSON_PATH))
    verify_release_state.add_argument("--live-url", default=DEFAULT_LIVE_MANIFEST_URL)
    verify_release_state.add_argument("--protocol", type=int)
    verify_release_state.set_defaults(func=cmd_verify_release_state)

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
