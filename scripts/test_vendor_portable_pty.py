from __future__ import annotations

import json
import subprocess
import unittest
from pathlib import Path


class VendorPortablePtyTests(unittest.TestCase):
    def test_vendored_tree_contains_required_upstream_files(self) -> None:
        root = Path(__file__).resolve().parent.parent / "vendor" / "portable-pty"
        required = [
            root / "Cargo.toml",
            root / "LICENSE.md",
            root / "src" / "lib.rs",
            root / "src" / "win" / "psuedocon.rs",
        ]

        missing = [str(path.relative_to(root)) for path in required if not path.exists()]
        self.assertEqual(missing, [])

    def test_cargo_patch_points_at_vendored_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        cargo_toml = (project_root / "Cargo.toml").read_text()

        self.assertIn('portable-pty = "=0.9.0"', cargo_toml)
        self.assertIn("[patch.crates-io]", cargo_toml)
        self.assertIn('portable-pty = { path = "vendor/portable-pty" }', cargo_toml)

    def test_cargo_metadata_resolves_portable_pty_to_vendored_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        result = subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=project_root,
            text=True,
            capture_output=True,
        )
        self.assertEqual(
            result.returncode,
            0,
            f"cargo metadata failed:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )

        metadata = json.loads(result.stdout)
        packages = [
            package
            for package in metadata["packages"]
            if package["name"] == "portable-pty" and package["version"] == "0.9.0"
        ]
        self.assertEqual(len(packages), 1)

        manifest_path = Path(packages[0]["manifest_path"]).resolve()
        expected = (project_root / "vendor" / "portable-pty" / "Cargo.toml").resolve()
        self.assertEqual(manifest_path, expected)

    def test_local_vendor_patches_are_listed_in_patch_index(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        index = project_root / "vendor" / "portable-pty.patches.md"
        patch_dir = project_root / "vendor" / "patches" / "portable-pty"
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

    def test_listed_local_vendor_patches_exist(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        index = project_root / "vendor" / "portable-pty.patches.md"
        text = index.read_text()
        listed = [
            line.split("`", 2)[1]
            for line in text.splitlines()
            if line.startswith("patch: `vendor/patches/portable-pty/")
        ]

        missing = [path for path in listed if not (project_root / path).exists()]
        self.assertEqual(missing, [])

    def test_local_vendor_patches_are_applied_to_vendored_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        patch_dir = project_root / "vendor" / "patches" / "portable-pty"

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

    def test_windows_conpty_loader_does_not_probe_path_conpty_dll(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        source = project_root / "vendor" / "portable-pty" / "src" / "win" / "psuedocon.rs"
        text = source.read_text()

        self.assertIn('ConPtyFuncs::open(Path::new("kernel32.dll"))', text)
        self.assertNotIn('Path::new("conpty.dll")', text)


if __name__ == "__main__":
    unittest.main()
