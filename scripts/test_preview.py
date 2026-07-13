import json
import os
import subprocess
import tempfile
import unittest
from unittest import mock
from pathlib import Path

import scripts.conventional_commits as conventional_commits
import scripts.preview as preview


class PreviewNotesTests(unittest.TestCase):
    def test_humanize_groups_conventional_subjects(self):
        self.assertEqual(
            preview.humanize_subject("feat(update): add preview channel"),
            ("Added", "Add preview channel"),
        )
        self.assertEqual(
            preview.humanize_subject("fix: handle preview manifest"),
            ("Fixed", "Handle preview manifest"),
        )
        self.assertEqual(
            preview.humanize_subject("not conventional"),
            ("Other", "Not conventional"),
        )

    def test_build_manifest_archives_current_assets(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "preview.json"
            notes = "Preview notes\n"
            content = preview.build_manifest(
                output=output,
                repo="ogulcancelik/herdr",
                tag="preview-2026-06-02-abcdef123456",
                build_id="2026-06-02-abcdef123456",
                commit="abcdef1234567890",
                built_at="2026-06-02T03:00:00Z",
                base_version="0.6.6",
                protocol=12,
                notes=notes,
                shas={"linux-x86_64": "deadbeef"},
                retain=30,
            )
            data = json.loads(content)
            self.assertEqual(data["channel"], "preview")
            self.assertEqual(data["build_id"], "2026-06-02-abcdef123456")
            self.assertEqual(
                data["assets"]["linux-x86_64"]["sha256"],
                "deadbeef",
            )
            self.assertEqual(
                data["assets"]["windows-x86_64"]["url"],
                "https://github.com/ogulcancelik/herdr/releases/download/preview-2026-06-02-abcdef123456/herdr-windows-x86_64.exe",
            )
            self.assertIn("2026-06-02-abcdef123456", data["builds"])

    def test_hidden_subjects_include_preview_manifest_commits(self):
        self.assertTrue(preview.hidden_subject("docs: update preview manifest"))
        self.assertTrue(preview.hidden_subject("docs: update website manifest"))
        self.assertFalse(preview.hidden_subject("release: v0.7.0"))
        self.assertFalse(preview.hidden_subject("fix: repair preview manifest"))

    def test_latest_publishable_commit_keeps_release_commits(self):
        output = "\n".join(
            [
                "manifest\x00docs: update website manifest for v0.7.0",
                "release\x00release: v0.7.0",
                "feature\x00feat: add plugin v1 system",
            ]
        )
        with mock.patch.object(preview, "run_git", return_value=output):
            self.assertEqual(preview.latest_publishable_commit("origin/master"), "release")

    def test_preview_range_base_advances_to_stable_tag(self):
        with (
            mock.patch.object(preview, "latest_stable_tag", return_value="v0.7.0"),
            mock.patch.object(preview, "git_is_ancestor", return_value=True),
        ):
            self.assertEqual(
                preview.preview_range_base("previous-preview", "release"),
                "v0.7.0",
            )

    def test_preview_range_base_keeps_previous_preview_for_unreleased_work(self):
        def is_ancestor(ancestor: str, descendant: str) -> bool:
            return (ancestor, descendant) == ("v0.7.0", "new-feature")

        with (
            mock.patch.object(preview, "latest_stable_tag", return_value="v0.7.0"),
            mock.patch.object(preview, "git_is_ancestor", side_effect=is_ancestor),
        ):
            self.assertEqual(
                preview.preview_range_base("previous-preview", "new-feature"),
                "previous-preview",
            )

    def test_post_stable_history_selects_release_and_bases_range_on_stable_tag(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)

            def git(*args: str) -> str:
                return subprocess.check_output(
                    ["git", *args],
                    cwd=repo,
                    text=True,
                    stderr=subprocess.DEVNULL,
                ).strip()

            git("init")
            git("config", "user.email", "test@example.com")
            git("config", "user.name", "Test User")

            marker = repo / "marker.txt"
            marker.write_text("preview\n", encoding="utf-8")
            git("add", "marker.txt")
            git("commit", "-m", "feat: previous preview")
            previous_preview = git("rev-parse", "HEAD")

            marker.write_text("release\n", encoding="utf-8")
            git("commit", "-am", "release: v0.7.0")
            release = git("rev-parse", "HEAD")
            git("tag", "v0.7.0")

            marker.write_text("manifest\n", encoding="utf-8")
            git("commit", "-am", "docs: update website manifest for v0.7.0")

            original_cwd = os.getcwd()
            try:
                os.chdir(repo)
                self.assertEqual(preview.latest_publishable_commit("HEAD"), release)
                self.assertEqual(
                    preview.preview_range_base(previous_preview, release),
                    "v0.7.0",
                )
            finally:
                os.chdir(original_cwd)

    def test_preview_docs_rewrite_links_to_preview_namespace(self):
        source = """---
title: Install Herdr
---

import ConfigReference from '../../components/ConfigReference.astro';
import LocaleWidget from '../../../components/LocaleWidget.astro';

[Install](/docs/install/)
file: ../../../public/assets/logo.svg
"""
        output = subprocess.check_output(
            ["node", "website/scripts/prepare-docs.mjs", "--rewrite-preview-doc-fixture"],
            input=source,
            text=True,
        )
        self.assertIn("[Install](/docs/preview/install/)", output)
        self.assertIn("file: ../../../../public/assets/logo.svg", output)
        self.assertIn("from '../../../components/ConfigReference.astro'", output)
        self.assertIn("from '../../../../components/LocaleWidget.astro'", output)
        self.assertIn("Preview docs describe unreleased preview builds", output)


class ConventionalCommitTests(unittest.TestCase):
    def test_valid_subjects_allow_scopes_and_bang(self):
        self.assertTrue(conventional_commits.valid_subject("fix(update): handle preview"))
        self.assertTrue(conventional_commits.valid_subject("feat!: change config"))
        self.assertFalse(conventional_commits.valid_subject("update preview channel"))

    def test_commit_message_subject_skips_comments(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "COMMIT_EDITMSG"
            path.write_text(
                "\n# Please enter the commit message\n\nfix(update): switch channel\n",
                encoding="utf-8",
            )
            self.assertEqual(
                conventional_commits.commit_message_subject(path),
                "fix(update): switch channel",
            )


if __name__ == "__main__":
    unittest.main()
