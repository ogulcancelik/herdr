from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import tarfile
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path


@dataclass
class VendorMetadata:
    source_repo: str
    source_commit: str
    dist_archive: str
    extracted_dir: str


def parse_archive_root(archive: Path) -> str:
    with tarfile.open(archive, "r:gz") as tar:
        roots = {
            member.name.split("/", 1)[0]
            for member in tar.getmembers()
            if member.name and member.name != "."
        }
    if len(roots) != 1:
        raise ValueError(f"expected exactly one archive root in {archive}, found {sorted(roots)}")
    return next(iter(roots))


def git_head(repo: Path) -> str:
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()


def ensure_dist_archive(source_repo: Path) -> Path:
    head = git_head(source_repo)[:9]
    subprocess.run(
        ["zig", "build", "dist", "-Demit-lib-vt", "-Doptimize=ReleaseFast"],
        cwd=source_repo,
        check=True,
    )
    dist_dir = source_repo / "zig-out" / "dist"
    archives = sorted(dist_dir.glob(f"libghostty-vt-*+{head}.tar.gz"))
    if not archives:
        raise FileNotFoundError(
            f"no libghostty-vt dist archive for HEAD {head} found in {dist_dir}"
        )
    return archives[-1]


def vendor_libghostty_vt(source_repo: Path, destination: Path) -> VendorMetadata:
    archive = ensure_dist_archive(source_repo)
    root = parse_archive_root(archive)

    with tempfile.TemporaryDirectory() as temp_dir:
        temp_dir_path = Path(temp_dir)
        with tarfile.open(archive, "r:gz") as tar:
            tar.extractall(temp_dir_path)

        extracted = temp_dir_path / root
        if not extracted.exists():
            raise FileNotFoundError(f"expected extracted root {extracted}")

        if destination.exists():
            shutil.rmtree(destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(extracted, destination)

    return VendorMetadata(
        source_repo=str(source_repo),
        source_commit=git_head(source_repo),
        dist_archive=archive.name,
        extracted_dir=root,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description="Vendor the pinned libghostty-vt source dist into herdr")
    parser.add_argument(
        "--source-repo",
        default="/home/can/Projects/ghostty",
        help="Path to a local ghostty checkout",
    )
    parser.add_argument(
        "--destination",
        default="vendor/libghostty-vt",
        help="Destination directory for the extracted libghostty-vt source dist",
    )
    parser.add_argument(
        "--metadata",
        default="vendor/libghostty-vt.vendor.json",
        help="Path to write vendoring metadata JSON",
    )
    args = parser.parse_args()

    repo = Path(args.source_repo).resolve()
    destination = Path(args.destination).resolve()
    metadata_path = Path(args.metadata).resolve()

    metadata = vendor_libghostty_vt(repo, destination)
    metadata_path.parent.mkdir(parents=True, exist_ok=True)
    metadata_path.write_text(json.dumps(asdict(metadata), indent=2) + "\n")

    print(f"vendored {metadata.extracted_dir} from {metadata.source_commit} into {destination}")


if __name__ == "__main__":
    main()
