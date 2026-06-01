from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from scripts.changelog import (
    ChangelogError,
    archived_releases_from_current_manifest,
    build_latest_json,
    canonicalize_manifest,
    DEFAULT_PRODUCT_ANNOUNCEMENT_PATH,
    default_release_assets,
    ensure_current_release_assets_are_mirrored,
    ensure_manifest_is_outdated,
    ensure_manifest_matches_expected,
    extract_section_body,
    infer_protocol_from_notes,
    load_product_announcement,
    manifest_from_release_payload,
    prepare_release,
    read_protocol_version,
)


class ChangelogScriptTests(unittest.TestCase):
    def test_prepare_release_moves_unreleased_into_versioned_section(self) -> None:
        original = """# Changelog\n\n## Unreleased\n\n### Fixed\n- Smoothed Claude flapping.\n\n## [0.1.0] - 2026-03-27\n\n### Added\n- Initial release.\n"""

        updated = prepare_release(original, "0.1.1", "2026-03-28")

        self.assertIn("## Unreleased\n\n## [0.1.1] - 2026-03-28", updated)
        self.assertIn("### Fixed\n- Smoothed Claude flapping.", updated)
        self.assertIn("## [0.1.0] - 2026-03-27", updated)

    def test_prepare_release_accepts_bracketed_unreleased_heading(self) -> None:
        original = """# Changelog\n\n## [Unreleased]\n\n### Added\n- Added sounds.\n"""

        updated = prepare_release(original, "0.1.1", "2026-03-28")

        self.assertIn("## Unreleased\n\n## [0.1.1] - 2026-03-28", updated)
        self.assertIn("### Added\n- Added sounds.", updated)

    def test_extract_section_body_returns_requested_version_only(self) -> None:
        changelog = """# Changelog\n\n## Unreleased\n\n## [0.1.1] - 2026-03-28\n\n### Fixed\n- Smoothed Claude flapping.\n\n## [0.1.0] - 2026-03-27\n\n### Added\n- Initial release.\n"""

        body = extract_section_body(changelog, "0.1.1")

        self.assertEqual(body, "### Fixed\n- Smoothed Claude flapping.\n")

    def test_build_latest_json_trims_notes(self) -> None:
        manifest = json.loads(
            build_latest_json(
                "0.1.1",
                "\n### Fixed\n- One\n\n",
                default_release_assets("0.1.1"),
            )
        )

        self.assertEqual(manifest["protocol"], read_protocol_version())
        self.assertEqual(manifest["notes"], "### Fixed\n- One")

    def test_build_latest_json_embeds_notes_and_release_assets(self) -> None:
        manifest = json.loads(
            build_latest_json(
                "v0.1.1",
                "### Fixed\n- Smoothed Claude flapping.\n",
                default_release_assets("0.1.1"),
            )
        )

        self.assertEqual(manifest["version"], "0.1.1")
        self.assertEqual(manifest["protocol"], read_protocol_version())
        self.assertEqual(manifest["notes"], "### Fixed\n- Smoothed Claude flapping.")
        self.assertEqual(
            manifest["assets"],
            {
                "linux-x86_64": "https://github.com/ogulcancelik/herdr/releases/download/v0.1.1/herdr-linux-x86_64",
                "linux-aarch64": "https://github.com/ogulcancelik/herdr/releases/download/v0.1.1/herdr-linux-aarch64",
                "macos-x86_64": "https://github.com/ogulcancelik/herdr/releases/download/v0.1.1/herdr-macos-x86_64",
                "macos-aarch64": "https://github.com/ogulcancelik/herdr/releases/download/v0.1.1/herdr-macos-aarch64",
            },
        )
        self.assertEqual(manifest["releases"]["0.1.1"]["assets"], manifest["assets"])

    def test_build_latest_json_embeds_product_announcement(self) -> None:
        manifest = json.loads(
            build_latest_json(
                "0.1.1",
                "### Fixed\n- One",
                default_release_assets("0.1.1"),
                announcement={"id": "keybinding-v2", "title": "Keybind Refactor", "body": "body"},
            )
        )

        self.assertEqual(
            manifest["announcement"],
            {"id": "keybinding-v2", "title": "Keybind Refactor", "body": "body"},
        )
        self.assertEqual(
            manifest["releases"]["0.1.1"]["announcement"],
            {"id": "keybinding-v2", "title": "Keybind Refactor", "body": "body"},
        )

    def test_build_latest_json_preserves_previous_release_metadata(self) -> None:
        manifest = json.loads(
            build_latest_json(
                "0.1.2",
                "### Fixed\n- Two",
                default_release_assets("0.1.2"),
                releases={"0.1.1": {"notes": "### Fixed\n- One"}},
            )
        )

        self.assertEqual(list(manifest["releases"]), ["0.1.2", "0.1.1"])
        self.assertEqual(manifest["releases"]["0.1.2"]["notes"], "### Fixed\n- Two")
        self.assertEqual(manifest["releases"]["0.1.2"]["protocol"], read_protocol_version())
        self.assertEqual(manifest["releases"]["0.1.2"]["assets"], default_release_assets("0.1.2"))
        self.assertEqual(manifest["releases"]["0.1.1"]["notes"], "### Fixed\n- One")
        self.assertEqual(manifest["releases"]["0.1.1"]["assets"], default_release_assets("0.1.1"))

    def test_build_latest_json_accepts_release_metadata_assets(self) -> None:
        assets = default_release_assets("0.1.1")
        manifest = json.loads(
            build_latest_json(
                "0.1.2",
                "### Fixed\n- Two",
                default_release_assets("0.1.2"),
                releases={"0.1.1": {"notes": "### Fixed\n- One", "assets": assets}},
            )
        )

        self.assertEqual(manifest["releases"]["0.1.1"]["assets"], assets)

    def test_build_latest_json_preserves_release_metadata_protocol(self) -> None:
        manifest = json.loads(
            build_latest_json(
                "0.1.2",
                "### Fixed\n- Two",
                default_release_assets("0.1.2"),
                releases={"0.1.1": {"notes": "### Fixed\n- One", "protocol": 7}},
            )
        )

        self.assertEqual(manifest["releases"]["0.1.1"]["protocol"], 7)

    def test_build_latest_json_infers_release_metadata_protocol_from_notes(self) -> None:
        manifest = json.loads(
            build_latest_json(
                "0.1.2",
                "### Fixed\n- Two",
                default_release_assets("0.1.2"),
                releases={
                    "0.1.1": {
                        "notes": "### Breaking Changes\n- The client/server protocol is now version 7."
                    }
                },
            )
        )

        self.assertEqual(manifest["releases"]["0.1.1"]["protocol"], 7)

    def test_archived_releases_from_current_manifest_seeds_legacy_root(self) -> None:
        releases = archived_releases_from_current_manifest(
            {
                "version": "0.1.1",
                "protocol": 3,
                "notes": "### Fixed\n- One",
                "announcement": {
                    "id": "one",
                    "title": "One",
                    "body": "body",
                },
            }
        )

        self.assertEqual(
            releases,
            {
                "0.1.1": {
                    "notes": "### Fixed\n- One",
                    "protocol": 3,
                    "assets": default_release_assets("0.1.1"),
                    "announcement": {
                        "id": "one",
                        "title": "One",
                        "body": "body",
                    },
                }
            },
        )

    def test_archived_releases_from_current_manifest_prefers_root_for_current_version(self) -> None:
        releases = archived_releases_from_current_manifest(
            {
                "version": "0.1.2",
                "protocol": read_protocol_version(),
                "notes": "### Fixed\n- Root",
                "releases": {
                    "0.1.2": {"notes": "### Fixed\n- Stale"},
                    "0.1.1": {"notes": "### Fixed\n- One"},
                },
            }
        )

        self.assertEqual(releases["0.1.2"]["notes"], "### Fixed\n- Root")
        self.assertEqual(releases["0.1.1"]["notes"], "### Fixed\n- One")
        self.assertEqual(releases["0.1.2"]["protocol"], read_protocol_version())
        self.assertEqual(releases["0.1.2"]["assets"], default_release_assets("0.1.2"))
        self.assertEqual(releases["0.1.1"]["assets"], default_release_assets("0.1.1"))

    def test_infer_protocol_from_notes(self) -> None:
        self.assertEqual(
            infer_protocol_from_notes("The client/server protocol is now version 10."),
            10,
        )
        self.assertEqual(
            infer_protocol_from_notes("The client/server protocol version 9."),
            9,
        )
        self.assertIsNone(infer_protocol_from_notes("No wire changes."))

    def write_temp_json(self, content: str) -> Path:
        tmp = tempfile.NamedTemporaryFile("w", delete=False, encoding="utf-8")
        with tmp:
            tmp.write(content)
        return Path(tmp.name)

    def test_checked_in_product_announcement_is_valid_or_null(self) -> None:
        self.assertTrue(DEFAULT_PRODUCT_ANNOUNCEMENT_PATH.is_file())
        load_product_announcement(DEFAULT_PRODUCT_ANNOUNCEMENT_PATH)

    def test_load_product_announcement_accepts_null(self) -> None:
        path = self.write_temp_json("null\n")
        try:
            self.assertIsNone(load_product_announcement(path))
        finally:
            path.unlink(missing_ok=True)

    def test_load_product_announcement_accepts_valid_object(self) -> None:
        path = self.write_temp_json(
            json.dumps({"id": "keybinding-v2", "title": "Keybind Refactor", "body": "Body"})
        )
        try:
            self.assertEqual(
                load_product_announcement(path),
                {"id": "keybinding-v2", "title": "Keybind Refactor", "body": "Body"},
            )
        finally:
            path.unlink(missing_ok=True)

    def test_load_product_announcement_rejects_missing_file(self) -> None:
        path = Path(tempfile.gettempdir()) / "herdr-missing-product-announcement.json"
        path.unlink(missing_ok=True)
        with self.assertRaisesRegex(ChangelogError, "file not found"):
            load_product_announcement(path)

    def test_load_product_announcement_rejects_missing_empty_or_extra_fields(self) -> None:
        cases = [
            ({"id": "keybinding-v2", "title": "Keybind Refactor"}, "body"),
            ({"id": "", "title": "Keybind Refactor", "body": "Body"}, "id"),
            ({"id": "keybinding-v2", "title": "Keybind Refactor", "body": "Body", "cta": "x"}, "unsupported"),
        ]
        for payload, expected in cases:
            path = self.write_temp_json(json.dumps(payload))
            try:
                with self.assertRaisesRegex(ChangelogError, expected):
                    load_product_announcement(path)
            finally:
                path.unlink(missing_ok=True)

    def test_load_product_announcement_rejects_invalid_id(self) -> None:
        path = self.write_temp_json(
            json.dumps({"id": "Keybinding V2", "title": "Keybind Refactor", "body": "Body"})
        )
        try:
            with self.assertRaisesRegex(ChangelogError, "invalid id"):
                load_product_announcement(path)
        finally:
            path.unlink(missing_ok=True)

    def test_manifest_from_release_payload_uses_release_body_and_asset_urls(self) -> None:
        manifest = manifest_from_release_payload(
            {
                "tagName": "v0.1.1",
                "isDraft": False,
                "isPrerelease": False,
                "body": "### Fixed\n- One\n",
                "assets": [
                    {"name": "herdr-linux-x86_64", "url": "https://example.com/linux-x86_64"},
                    {"name": "herdr-linux-aarch64", "url": "https://example.com/linux-aarch64"},
                    {"name": "herdr-macos-x86_64", "url": "https://example.com/macos-x86_64"},
                    {"name": "herdr-macos-aarch64", "url": "https://example.com/macos-aarch64"},
                ],
            },
            "0.1.1",
        )

        self.assertEqual(
            manifest,
            {
                "version": "0.1.1",
                "protocol": read_protocol_version(),
                "notes": "### Fixed\n- One",
                "assets": {
                    "linux-x86_64": "https://example.com/linux-x86_64",
                    "linux-aarch64": "https://example.com/linux-aarch64",
                    "macos-x86_64": "https://example.com/macos-x86_64",
                    "macos-aarch64": "https://example.com/macos-aarch64",
                },
            },
        )

    def test_manifest_from_release_payload_uses_explicit_protocol(self) -> None:
        manifest = manifest_from_release_payload(
            {
                "tagName": "v0.1.1",
                "isDraft": False,
                "isPrerelease": False,
                "body": "### Fixed\n- One\n",
                "assets": [
                    {"name": "herdr-linux-x86_64", "url": "https://example.com/linux-x86_64"},
                    {"name": "herdr-linux-aarch64", "url": "https://example.com/linux-aarch64"},
                    {"name": "herdr-macos-x86_64", "url": "https://example.com/macos-x86_64"},
                    {"name": "herdr-macos-aarch64", "url": "https://example.com/macos-aarch64"},
                ],
            },
            "0.1.1",
            protocol=42,
        )

        self.assertEqual(manifest["protocol"], 42)

    def test_manifest_from_release_payload_rejects_missing_asset(self) -> None:
        with self.assertRaisesRegex(ChangelogError, "missing asset herdr-macos-aarch64"):
            manifest_from_release_payload(
                {
                    "tagName": "v0.1.1",
                    "isDraft": False,
                    "isPrerelease": False,
                    "body": "### Fixed\n- One\n",
                    "assets": [
                        {"name": "herdr-linux-x86_64", "url": "https://example.com/linux-x86_64"},
                        {"name": "herdr-linux-aarch64", "url": "https://example.com/linux-aarch64"},
                        {"name": "herdr-macos-x86_64", "url": "https://example.com/macos-x86_64"},
                    ],
                },
                "0.1.1",
            )

    def test_ensure_manifest_is_outdated_rejects_same_or_newer_version(self) -> None:
        with self.assertRaisesRegex(ChangelogError, "already at v0.1.1"):
            ensure_manifest_is_outdated({"version": "0.1.1"}, "0.1.1")

        with self.assertRaisesRegex(ChangelogError, "already at v0.1.2"):
            ensure_manifest_is_outdated({"version": "0.1.2"}, "0.1.1")

    def test_ensure_manifest_is_outdated_allows_older_version(self) -> None:
        ensure_manifest_is_outdated({"version": "0.1.0"}, "0.1.1")

    def test_canonicalize_manifest_requires_all_asset_targets(self) -> None:
        with self.assertRaisesRegex(ChangelogError, "missing asset URL for macos-aarch64"):
            canonicalize_manifest(
                {
                    "version": "0.1.1",
                    "protocol": read_protocol_version(),
                    "notes": "### Fixed\n- One",
                    "assets": {
                        "linux-x86_64": "https://example.com/linux-x86_64",
                        "linux-aarch64": "https://example.com/linux-aarch64",
                        "macos-x86_64": "https://example.com/macos-x86_64",
                    },
                },
                "test manifest",
            )

    def test_ensure_manifest_matches_expected_normalizes_whitespace(self) -> None:
        actual = {
            "version": "v0.1.1",
            "protocol": read_protocol_version(),
            "notes": "\n### Fixed\n- One\n",
            "assets": {
                "linux-x86_64": " https://example.com/linux-x86_64 ",
                "linux-aarch64": "https://example.com/linux-aarch64",
                "macos-x86_64": "https://example.com/macos-x86_64",
                "macos-aarch64": "https://example.com/macos-aarch64",
            },
        }
        expected = {
            "version": "0.1.1",
            "protocol": read_protocol_version(),
            "notes": "### Fixed\n- One",
            "assets": {
                "linux-x86_64": "https://example.com/linux-x86_64",
                "linux-aarch64": "https://example.com/linux-aarch64",
                "macos-x86_64": "https://example.com/macos-x86_64",
                "macos-aarch64": "https://example.com/macos-aarch64",
            },
        }

        canonical = ensure_manifest_matches_expected(actual, expected, "test manifest")
        self.assertEqual(canonical, expected)

    def test_current_release_assets_must_be_mirrored(self) -> None:
        assets = default_release_assets("0.1.1")
        ensure_current_release_assets_are_mirrored(
            {
                "version": "0.1.1",
                "protocol": read_protocol_version(),
                "notes": "### Fixed\n- One",
                "assets": assets,
                "releases": {
                    "0.1.1": {
                        "notes": "### Fixed\n- One",
                        "assets": assets,
                    }
                },
            },
            "test manifest",
        )

    def test_current_release_assets_must_match_top_level_assets(self) -> None:
        with self.assertRaisesRegex(ChangelogError, "assets must match top-level assets"):
            ensure_current_release_assets_are_mirrored(
                {
                    "version": "0.1.1",
                    "protocol": read_protocol_version(),
                    "notes": "### Fixed\n- One",
                    "assets": default_release_assets("0.1.1"),
                    "releases": {
                        "0.1.1": {
                            "notes": "### Fixed\n- One",
                            "assets": default_release_assets("0.1.0"),
                        }
                    },
                },
                "test manifest",
            )

    def test_ensure_manifest_matches_expected_rejects_different_notes(self) -> None:
        with self.assertRaisesRegex(ChangelogError, "does not match the published GitHub release manifest"):
            ensure_manifest_matches_expected(
                {
                    "version": "0.1.1",
                    "protocol": read_protocol_version(),
                    "notes": "### Fixed\n- Different",
                    "assets": {
                        "linux-x86_64": "https://example.com/linux-x86_64",
                        "linux-aarch64": "https://example.com/linux-aarch64",
                        "macos-x86_64": "https://example.com/macos-x86_64",
                        "macos-aarch64": "https://example.com/macos-aarch64",
                    },
                },
                {
                    "version": "0.1.1",
                    "protocol": read_protocol_version(),
                    "notes": "### Fixed\n- One",
                    "assets": {
                        "linux-x86_64": "https://example.com/linux-x86_64",
                        "linux-aarch64": "https://example.com/linux-aarch64",
                        "macos-x86_64": "https://example.com/macos-x86_64",
                        "macos-aarch64": "https://example.com/macos-aarch64",
                    },
                },
                "test manifest",
            )

    def test_canonicalize_manifest_requires_protocol(self) -> None:
        with self.assertRaisesRegex(ChangelogError, "missing an integer protocol"):
            canonicalize_manifest(
                {
                    "version": "0.1.1",
                    "notes": "### Fixed\n- One",
                    "assets": default_release_assets("0.1.1"),
                },
                "test manifest",
            )

    def test_ensure_manifest_matches_expected_rejects_different_protocol(self) -> None:
        actual = {
            "version": "0.1.1",
            "protocol": read_protocol_version() + 1,
            "notes": "### Fixed\n- One",
            "assets": default_release_assets("0.1.1"),
        }
        expected = {
            "version": "0.1.1",
            "protocol": read_protocol_version(),
            "notes": "### Fixed\n- One",
            "assets": default_release_assets("0.1.1"),
        }

        with self.assertRaisesRegex(ChangelogError, "does not match"):
            ensure_manifest_matches_expected(actual, expected, "test manifest")


if __name__ == "__main__":
    unittest.main()
