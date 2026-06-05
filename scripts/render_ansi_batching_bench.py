#!/usr/bin/env python3
"""Local before/after harness for diff-render ANSI batching.

The harness intentionally avoids Cargo/build.rs so it can run on machines that do
not have Zig installed. It mirrors the old per-cell CUP diff encoder and the new
contiguous-run CUP diff encoder closely enough to compare the scrollback-heavy
case from issue #283.
"""

from __future__ import annotations

import argparse
import dataclasses
import statistics
import time
from typing import Iterable, Literal

RESET_FG = 0x00_00_00_00
RESET_BG = 0x00_00_00_00
RED = 0x00_00_00_02
GREEN = 0x00_00_00_03
YELLOW = 0x00_00_00_04
BLUE = 0x00_00_00_05
MAGENTA = 0x00_00_00_06
CYAN = 0x00_00_00_07
BOLD = 1 << 0
ITALIC = 1 << 2


@dataclasses.dataclass(frozen=True)
class Cell:
    symbol: str
    fg: int = RESET_FG
    bg: int = RESET_BG
    modifier: int = 0
    skip: bool = False


def color_to_sgr_fg(value: int) -> str:
    named = {
        RESET_FG: "39",
        0x00_00_00_01: "30",
        RED: "31",
        GREEN: "32",
        YELLOW: "33",
        BLUE: "34",
        MAGENTA: "35",
        CYAN: "36",
        0x00_00_00_08: "37",
    }
    return named.get(value, "39")


def color_to_sgr_bg(value: int) -> str:
    return "49" if value == RESET_BG else "49"


def build_sgr(cell: Cell) -> str:
    parts = ["0"]
    if cell.modifier & BOLD:
        parts.append("1")
    if cell.modifier & ITALIC:
        parts.append("3")
    parts.append(color_to_sgr_fg(cell.fg))
    parts.append(color_to_sgr_bg(cell.bg))
    return f"\x1b[{';'.join(parts)}m"


def line_cells(line_no: int, width: int) -> list[Cell]:
    level = ("INFO", "DEBUG", "WARN", "ERROR", "TRACE")[line_no % 5]
    fg = {"INFO": GREEN, "DEBUG": CYAN, "WARN": YELLOW, "ERROR": RED, "TRACE": MAGENTA}[level]
    mod = BOLD if level in {"WARN", "ERROR"} else 0
    alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_./:-"
    noisy_tail = "".join(
        alphabet[(line_no * 131 + idx * 17 + idx * idx) % len(alphabet)]
        for idx in range(width)
    )
    message = (
        f"2026-06-05T17:{line_no % 60:02d}:{(line_no * 7) % 60:02d}.{line_no % 1000:03d}Z "
        f"{level:<5} oc[{line_no % 17:02d}] "
        f"s={line_no:08x} t={line_no:07d} payload={noisy_tail}"
    )
    cells: list[Cell] = []
    for idx, ch in enumerate(message[:width].ljust(width)):
        if idx < 29:
            cells.append(Cell(ch, BLUE, RESET_BG, 0))
        elif idx < 35:
            cells.append(Cell(ch, fg, RESET_BG, mod))
        elif "0" <= ch <= "9" and idx % 3 == 0:
            cells.append(Cell(ch, MAGENTA, RESET_BG, 0))
        else:
            cells.append(Cell(ch, RESET_FG, RESET_BG, 0))
    return cells


def generated_scrollback(target_mb: int, width: int) -> tuple[int, list[list[Cell]]]:
    target_bytes = target_mb * 1024 * 1024
    source_bytes = 0
    lines: list[list[Cell]] = []
    line_no = 0
    while source_bytes < target_bytes:
        cells = line_cells(line_no, width)
        # Approximate source scrollback size including ANSI/style bytes and newline.
        visible = "".join(cell.symbol for cell in cells).rstrip()
        source_bytes += len(visible.encode()) + 24 + 1
        lines.append(cells)
        line_no += 1
    return source_bytes, lines


def file_scrollback(path: str, width: int) -> tuple[int, list[list[Cell]]]:
    source = open(path, "rb").read()
    lines: list[list[Cell]] = []
    for raw in source.splitlines() or [b""]:
        text = raw.decode("utf-8", errors="replace").replace("\t", "    ")
        cells = [Cell(ch) for ch in text[:width].ljust(width)]
        lines.append(cells)
    return len(source), lines


def frame(lines: list[list[Cell]], offset: int, height: int) -> list[Cell]:
    return [cell for row in lines[offset : offset + height] for cell in row]


def write_cell_at_cursor(cell: Cell, last_sgr: str) -> tuple[int, str]:
    written = 0
    sgr = build_sgr(cell)
    if sgr != last_sgr:
        written += len(sgr.encode())
        last_sgr = sgr
    written += len(cell.symbol.encode())
    return written, last_sgr


def encode_diff(
    prev: list[Cell],
    curr: list[Cell],
    width: int,
    height: int,
    mode: Literal["baseline", "fixed", "scroll"],
) -> tuple[int, int, int]:
    # Synchronized output, hide cursor, close inherited OSC 8.
    bytes_written = len(b"\x1b[?2026h\x1b[?25l\x1b]8;;\x1b\\")
    cups = 0
    chunks = 0
    last_sgr = ""

    if mode == "scroll" and height > 1 and all(
        curr[row * width : (row + 1) * width]
        == prev[(row + 1) * width : (row + 2) * width]
        for row in range(height - 1)
    ):
        bytes_written += len(f"\x1b[1;{height}r".encode()) + len(b"\x1b[1S") + len(b"\x1b[r")
        cups += 0
        chunks += 1
        row = height - 1
        for col in range(width):
            cell = curr[row * width + col]
            if col == 0:
                cup = f"\x1b[{row + 1};{col + 1}H".encode()
                bytes_written += len(cup)
                cups += 1
                chunks += 1
            written, last_sgr = write_cell_at_cursor(cell, last_sgr)
            bytes_written += written
        if last_sgr:
            bytes_written += len(b"\x1b[0m")
        for seq in (f"\x1b[{height};1H".encode(), b"\x1b[?2026l", f"\x1b[{height};1H".encode()):
            bytes_written += len(seq)
        cups += 2
        return bytes_written, cups, chunks

    for row in range(height):
        run_next_col: int | None = None
        for col in range(width):
            idx = row * width + col
            cell = curr[idx]
            if cell.skip or cell == prev[idx]:
                if mode == "fixed":
                    run_next_col = None
                continue

            if mode == "baseline" or run_next_col != col:
                cup = f"\x1b[{row + 1};{col + 1}H".encode()
                bytes_written += len(cup)
                cups += 1
                chunks += 1

            written, last_sgr = write_cell_at_cursor(cell, last_sgr)
            bytes_written += written
            if mode in {"fixed", "scroll"}:
                run_next_col = col + 1

    if last_sgr:
        bytes_written += len(b"\x1b[0m")
    # Hidden host cursor position, end sync, IME anchor position.
    for seq in (f"\x1b[{height};1H".encode(), b"\x1b[?2026l", f"\x1b[{height};1H".encode()):
        bytes_written += len(seq)
    cups += 2
    return bytes_written, cups, chunks


def sample_offsets(line_count: int, height: int, samples: int) -> list[int]:
    max_offset = max(1, line_count - height - 1)
    if samples >= max_offset:
        return list(range(max_offset))
    return sorted({round(i * (max_offset - 1) / (samples - 1)) for i in range(samples)})


def measure_lines(source_bytes: int, lines: list[list[Cell]], target_name: str, width: int, height: int, samples: int, repeat: int) -> dict[str, object]:
    offsets = sample_offsets(len(lines), height, samples)
    result: dict[str, object] = {
        "target": target_name,
        "source_bytes": source_bytes,
        "lines": len(lines),
        "samples": len(offsets),
    }

    for mode in ("baseline", "fixed", "scroll"):
        encoded_bytes: list[int] = []
        cups: list[int] = []
        chunks: list[int] = []
        started = time.perf_counter()
        for _ in range(repeat):
            for offset in offsets:
                prev = frame(lines, offset, height)
                curr = frame(lines, offset + 1, height)
                b, c, k = encode_diff(prev, curr, width, height, mode)  # type: ignore[arg-type]
                encoded_bytes.append(b)
                cups.append(c)
                chunks.append(k)
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        result[mode] = {
            "bytes_avg": round(statistics.fmean(encoded_bytes)),
            "bytes_total": sum(encoded_bytes),
            "cups_avg": round(statistics.fmean(cups)),
            "chunks_avg": round(statistics.fmean(chunks)),
            "elapsed_ms": round(elapsed_ms, 2),
        }
    return result


def measure(target_mb: int, width: int, height: int, samples: int, repeat: int) -> dict[str, object]:
    source_bytes, lines = generated_scrollback(target_mb, width)
    return measure_lines(source_bytes, lines, f"{target_mb}MB-synth", width, height, samples, repeat)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sizes-mb", default="5,10")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=40)
    parser.add_argument("--samples", type=int, default=200)
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--log-file")
    args = parser.parse_args()

    print(
        "target source_bytes lines samples mode avg_bytes avg_CUP avg_chunks elapsed_ms improvement_bytes improvement_CUP improvement_elapsed"
    )
    rows = []
    if args.log_file:
        source_bytes, lines = file_scrollback(args.log_file, args.width)
        rows.append(measure_lines(source_bytes, lines, "gateway.log", args.width, args.height, args.samples, args.repeat))
    else:
        for size in [int(part) for part in args.sizes_mb.split(",") if part.strip()]:
            rows.append(measure(size, args.width, args.height, args.samples, args.repeat))

    for row in rows:
        baseline = row["baseline"]  # type: ignore[index]
        fixed = row["fixed"]  # type: ignore[index]
        scroll = row["scroll"]  # type: ignore[index]
        for mode, metrics in (("baseline", baseline), ("fixed", fixed), ("scroll", scroll)):
            improvement_bytes = "-"
            improvement_cup = "-"
            improvement_elapsed = "-"
            if mode != "baseline":
                improvement_bytes = f"{baseline['bytes_avg'] / metrics['bytes_avg']:.2f}x"
                improvement_cup = f"{baseline['cups_avg'] / metrics['cups_avg']:.2f}x"
                improvement_elapsed = f"{baseline['elapsed_ms'] / metrics['elapsed_ms']:.2f}x"
            print(
                row["target"],
                row["source_bytes"],
                row["lines"],
                row["samples"],
                mode,
                metrics["bytes_avg"],
                metrics["cups_avg"],
                metrics["chunks_avg"],
                metrics["elapsed_ms"],
                improvement_bytes,
                improvement_cup,
                improvement_elapsed,
            )


if __name__ == "__main__":
    main()
