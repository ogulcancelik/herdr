from __future__ import annotations

import io
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts.vendor_libghostty_vt import (
    ensure_dist_archive,
    parse_archive_root,
    require_clean_checkout,
)


class VendorLibghosttyVtTests(unittest.TestCase):
    def test_parse_archive_root_returns_single_top_level_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            archive = Path(temp_dir) / "libghostty-vt.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                data = b"hello"
                info = tarfile.TarInfo("libghostty-vt-1.0.0/README.md")
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))

            self.assertEqual(parse_archive_root(archive), "libghostty-vt-1.0.0")

    def test_ensure_dist_archive_refuses_stale_archives_without_head_match(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            dist = repo / "zig-out" / "dist"
            dist.mkdir(parents=True)
            (dist / "libghostty-vt-1.3.2-main-+deadbeef0.tar.gz").write_bytes(b"stale")

            def git_output(command: list[str], **_kwargs: object) -> str:
                if command[1] == "status":
                    return ""
                return "0123456789abcdef\n"

            with (
                mock.patch("scripts.vendor_libghostty_vt.subprocess.run"),
                mock.patch(
                    "scripts.vendor_libghostty_vt.subprocess.check_output",
                    side_effect=git_output,
                ),
            ):
                with self.assertRaisesRegex(FileNotFoundError, "HEAD 012345678"):
                    ensure_dist_archive(repo)

    def test_require_clean_checkout_rejects_tracked_and_untracked_changes(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            with mock.patch(
                "scripts.vendor_libghostty_vt.subprocess.check_output",
                return_value=" M src/terminal.zig\n?? local.patch\n",
            ):
                with self.assertRaisesRegex(ValueError, "refusing to vendor from dirty checkout"):
                    require_clean_checkout(repo)

    def test_ensure_dist_archive_rejects_checkout_dirtied_by_build(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            with (
                mock.patch("scripts.vendor_libghostty_vt.subprocess.run"),
                mock.patch(
                    "scripts.vendor_libghostty_vt.subprocess.check_output",
                    side_effect=["", "0123456789abcdef\n", " M generated.txt\n"],
                ),
            ):
                with self.assertRaisesRegex(ValueError, "refusing to vendor from dirty checkout"):
                    ensure_dist_archive(repo)

    def test_vendored_tree_contains_required_upstream_files(self) -> None:
        root = Path(__file__).resolve().parent.parent / "vendor" / "libghostty-vt"
        required = [
            root / "build.zig",
            root / "build.zig.zon",
            root / "CMakeLists.txt",
            root / "dist" / "cmake" / "ghostty-vt-config.cmake.in",
            root / "include" / "ghostty" / "vt.h",
            root / "include" / "ghostty" / "vt" / "render.h",
            root / "src" / "lib_vt.zig",
        ]

        missing = [str(path.relative_to(root)) for path in required if not path.exists()]
        self.assertEqual(missing, [])

    def test_vendor_metadata_exists_and_points_at_vendored_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        metadata = project_root / "vendor" / "libghostty-vt.vendor.json"
        self.assertTrue(metadata.exists())
        text = metadata.read_text()
        self.assertIn('"source_commit"', text)
        self.assertIn('"dist_archive"', text)
        self.assertIn('"extracted_dir"', text)

    def test_local_vendor_patches_are_listed_in_patch_index(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        index = project_root / "vendor" / "libghostty-vt.patches.md"
        patch_dir = project_root / "vendor" / "patches" / "libghostty-vt"
        patches = sorted(patch_dir.glob("*.patch"))

        if not patches:
            return

        self.assertTrue(index.exists())
        text = index.read_text()
        missing = [
            str(path.relative_to(project_root))
            for path in patches
            if str(path.relative_to(project_root)) not in text
        ]
        self.assertEqual(missing, [])

    def test_local_vendor_patches_are_applied_to_vendored_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        patch_dir = project_root / "vendor" / "patches" / "libghostty-vt"

        for patch in sorted(patch_dir.glob("*.patch")):
            result = subprocess.run(
                ["git", "apply", "--check", "--reverse", str(patch.relative_to(project_root))],
                cwd=project_root,
                text=True,
                capture_output=True,
            )
            self.assertEqual(
                result.returncode,
                0,
                f"{patch.relative_to(project_root)} is not applied cleanly:\n"
                f"stdout:\n{result.stdout}\n"
                f"stderr:\n{result.stderr}",
            )

    def test_embedded_libghostty_logging_is_silenced(self) -> None:
        root = Path(__file__).resolve().parent.parent / "vendor" / "libghostty-vt"
        lib_vt = root / "src" / "lib_vt.zig"
        sys_zig = root / "src" / "terminal" / "c" / "sys.zig"
        lib_text = lib_vt.read_text()
        sys_text = sys_zig.read_text()
        self.assertIn('.logFn = @import("terminal/c/sys.zig").logFn', lib_text)
        self.assertIn("if (global.log == null) return;", sys_text)


if __name__ == "__main__":
    unittest.main()
