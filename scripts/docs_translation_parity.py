from __future__ import annotations

import argparse
import sys
from pathlib import Path


DEFAULT_LOCALES = ("ja", "zh-cn")


def heading_outline(path: Path) -> list[int]:
    outline: list[int] = []
    in_fence = False

    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.lstrip()
        if stripped.startswith("```") or stripped.startswith("~~~"):
            in_fence = not in_fence
            continue
        if in_fence or not stripped.startswith("#"):
            continue

        level = 0
        for char in stripped:
            if char != "#":
                break
            level += 1

        if level == 0 or level > 6:
            continue
        if len(stripped) > level and stripped[level] not in (" ", "\t"):
            continue

        outline.append(level)

    return outline


def english_docs(docs_root: Path) -> list[Path]:
    return sorted(
        path
        for path in docs_root.glob("*.mdx")
        if path.is_file()
    )


def locale_docs(docs_root: Path, locale: str) -> list[Path]:
    locale_root = docs_root / locale
    if not locale_root.exists():
        return []
    return sorted(path for path in locale_root.glob("*.mdx") if path.is_file())


def check_docs_translation_parity(docs_root: Path, locales: tuple[str, ...] = DEFAULT_LOCALES) -> list[str]:
    errors: list[str] = []
    english = english_docs(docs_root)
    english_names = {path.name for path in english}

    for locale in locales:
        translated_names = {path.name for path in locale_docs(docs_root, locale)}

        for missing in sorted(english_names - translated_names):
            errors.append(f"{docs_root / locale / missing}: missing translation file")

        for stale in sorted(translated_names - english_names):
            errors.append(f"{docs_root / locale / stale}: no matching English doc")

    for source in english:
        source_outline = heading_outline(source)

        for locale in locales:
            translated = docs_root / locale / source.name
            if not translated.exists():
                continue

            translated_outline = heading_outline(translated)
            if translated_outline == source_outline:
                continue

            errors.append(
                format_outline_error(source, translated, source_outline, translated_outline)
            )

    return errors


def format_outline_error(
    source: Path,
    translated: Path,
    source_outline: list[int],
    translated_outline: list[int],
) -> str:
    return (
        f"{translated}: heading outline differs from {source} "
        f"(English {format_counts(source_outline)}, translated {format_counts(translated_outline)})"
    )


def format_counts(levels: list[int]) -> str:
    if not levels:
        return "0 headings"

    parts = []
    for level in range(1, 7):
        count = levels.count(level)
        if count:
            parts.append(f"h{level}={count}")
    return ", ".join(parts)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Check localized docs have the same heading outline as English docs."
    )
    parser.add_argument(
        "--docs-root",
        default="website/src/content/docs",
        type=Path,
        help="Docs content root containing English .mdx files and locale subdirectories.",
    )
    parser.add_argument(
        "--locale",
        action="append",
        dest="locales",
        help="Locale subdirectory to check. Can be passed more than once.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    locales = tuple(args.locales or DEFAULT_LOCALES)
    errors = check_docs_translation_parity(args.docs_root, locales)

    if errors:
        print("error: localized docs heading outlines differ from English docs", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
