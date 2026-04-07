from __future__ import annotations

import json
import unittest

from scripts.changelog import (
    ChangelogError,
    build_latest_json,
    default_release_assets,
    ensure_manifest_is_outdated,
    extract_section_body,
    manifest_from_release_payload,
    prepare_release,
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
                "notes": "### Fixed\n- One",
                "assets": {
                    "linux-x86_64": "https://example.com/linux-x86_64",
                    "linux-aarch64": "https://example.com/linux-aarch64",
                    "macos-x86_64": "https://example.com/macos-x86_64",
                    "macos-aarch64": "https://example.com/macos-aarch64",
                },
            },
        )

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


if __name__ == "__main__":
    unittest.main()
