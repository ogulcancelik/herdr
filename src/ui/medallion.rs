//! Status medallion: nested rectangular bands rasterized into sub-cell block
//! characters for the sidebar's two-line server rows (issue #42).
//!
//! The medallion occupies a 3-cell-wide x 2-line-tall block — the densest
//! solid-block canvas the two-line row allows. Sub-cell resolution comes from
//! block-mosaic glyphs:
//!
//! * [`MedallionStyle::Sextant`] — 2x3 sub-blocks per cell (Unicode "Symbols
//!   for Legacy Computing" sextants, U+1FB00..=U+1FB3B) for a 6x6 grid.
//! * [`MedallionStyle::Quadrant`] — 2x2 sub-blocks per cell (quadrant blocks,
//!   U+2580..=U+259F) for a 6x4 grid. Safer font coverage; the caller picks.
//!
//! The shape is RECTANGULAR by design (issue #42 refinement): nested
//! rectangle borders with band widths outer/middle/core = 1/1/2 — corners
//! belong to the outer band, no rounding, no base-colored pixels. `rings` is
//! ordered OUTER -> INNER (severity-sorted by the caller: the worst state is
//! the perimeter).
//!
//! ```text
//!   Sextant (6x6)            Quadrant (6x4, no room for a middle row:
//!     o o o o o o             the middle band is vertical-only)
//!     o m m m m o              o o o o o o
//!     o m c c m o              o m c c m o
//!     o m c c m o              o m c c m o
//!     o m m m m o              o o o o o o
//!     o o o o o o
//! ```
//!
//! ## The approximation
//!
//! A terminal cell carries exactly one fg + one bg. Corner cells contain two
//! band colors (outer + middle) — exact: inner band sub-blocks lit as fg,
//! outer as bg. The two vertical-center cells contain THREE band colors
//! (outer, middle, core) for two slots; resolution: the innermost color wins
//! fg (its sub-blocks lit), bg goes to the OUTER color — perimeter
//! continuity beats the middle band, which visually breaks at the top/bottom
//! center. Documented, deliberate.
//!
//! Degrades: two rings = outer border + filled inner; one ring = a solid
//! rectangle; empty = blank `base_bg` spans (the only place `base_bg` shows —
//! the medallion proper is fully opaque). Duplicate colors merge into solid
//! regions. At most three rings are used; extra (inner) entries are ignored.

use ratatui::style::{Color, Style};
use ratatui::text::Span;

/// Medallion width in terminal cells (per line; the medallion is two lines
/// tall).
pub(crate) const MEDALLION_WIDTH: u16 = 3;

/// Sub-cell raster resolution for [`ring_medallion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MedallionStyle {
    /// 2x3 sub-blocks per cell via U+1FB00..=U+1FB3B sextants (plus half/full
    /// blocks for the patterns Unicode carves out of that range).
    Sextant,
    /// 2x2 sub-blocks per cell via U+2580..=U+259F quadrant blocks. Fallback
    /// for fonts without Symbols for Legacy Computing coverage.
    Quadrant,
}

/// Renders the medallion for `rings` (ordered OUTER -> INNER, severity-sorted
/// by the caller, at most three used) as two lines of three 1-cell spans
/// each. `base_bg` shows only when `rings` is empty (blank spans); the
/// medallion itself is fully opaque.
pub(crate) fn ring_medallion(
    rings: &[Color],
    base_bg: Color,
    style: MedallionStyle,
) -> [Vec<Span<'static>>; 2] {
    let rings = &rings[..rings.len().min(3)];
    if rings.is_empty() {
        let blank_line =
            || vec![Span::styled(" ", Style::default().bg(base_bg)); MEDALLION_WIDTH as usize];
        return [blank_line(), blank_line()];
    }

    let mut lines = [Vec::with_capacity(3), Vec::with_capacity(3)];
    for (cell_y, line) in lines.iter_mut().enumerate() {
        for cell_x in 0..MEDALLION_WIDTH as usize {
            let (glyph, fg, bg) = render_cell(style, cell_x, cell_y, rings);
            line.push(Span::styled(
                glyph.to_string(),
                Style::default().fg(fg).bg(bg),
            ));
        }
    }
    lines
}

/// (columns, rows) of sub-blocks within one terminal cell.
fn cell_sub_dims(style: MedallionStyle) -> (usize, usize) {
    match style {
        MedallionStyle::Sextant => (2, 3),
        MedallionStyle::Quadrant => (2, 2),
    }
}

/// Band index of one sub-pixel: 0 = outer border, 1 = middle band, 2 = core.
/// Nested rectangles by border distance; the quadrant grid (6x4) has no
/// vertical room for a middle row, so its middle band is vertical-only.
fn band_index(style: MedallionStyle, x: usize, y: usize) -> usize {
    let (sub_cols, sub_rows) = cell_sub_dims(style);
    let (cols, rows) = (
        sub_cols * MEDALLION_WIDTH as usize,
        sub_rows * 2, // two lines tall
    );
    let d = x.min(y).min(cols - 1 - x).min(rows - 1 - y);
    match style {
        MedallionStyle::Sextant => d.min(2),
        MedallionStyle::Quadrant => {
            if d == 0 {
                0
            } else if (2..=3).contains(&x) {
                2
            } else {
                1
            }
        }
    }
}

/// Maps a band index onto the available ring colors: with two rings the
/// middle band joins the core (outer border + filled inner); with one, the
/// whole medallion is that color.
fn color_index(band: usize, ring_count: usize) -> usize {
    band.min(ring_count - 1)
}

/// One cell's (glyph, fg, bg): sub-blocks of the cell's INNERMOST color are
/// lit as fg over a bg of its outermost color. Cells crossing three colors
/// elect: innermost wins fg, bg goes to the outer color (perimeter
/// continuity; the middle band breaks there — see module docs).
fn render_cell(
    style: MedallionStyle,
    cell_x: usize,
    cell_y: usize,
    rings: &[Color],
) -> (char, Color, Color) {
    let (sub_cols, sub_rows) = cell_sub_dims(style);
    let mut deepest = 0usize;
    let mut shallowest = usize::MAX;
    for sy in 0..sub_rows {
        for sx in 0..sub_cols {
            let band = band_index(style, cell_x * sub_cols + sx, cell_y * sub_rows + sy);
            let color = color_index(band, rings.len());
            deepest = deepest.max(color);
            shallowest = shallowest.min(color);
        }
    }

    let fg_color = rings[deepest];
    let bg_color = rings[shallowest];
    if fg_color == bg_color {
        // One color (single ring, or duplicates merging): solid block.
        return ('\u{2588}', fg_color, bg_color);
    }

    // Light the fg color's sub-blocks (by COLOR equality, so duplicate ring
    // colors merge into coherent regions) over the shallowest as bg.
    let mut bits = 0u8;
    for sy in 0..sub_rows {
        for sx in 0..sub_cols {
            let band = band_index(style, cell_x * sub_cols + sx, cell_y * sub_rows + sy);
            if rings[color_index(band, rings.len())] == fg_color {
                bits |= 1 << (sy * sub_cols + sx);
            }
        }
    }
    let glyph = match style {
        MedallionStyle::Sextant => sextant_char(bits),
        MedallionStyle::Quadrant => quadrant_char(bits),
    };
    (glyph, fg_color, bg_color)
}

/// Sextant glyph for a 2x3 bitmask (bit 0 = upper-left .. bit 5 =
/// lower-right). Unicode carves the empty, full, and half-column patterns out
/// of the U+1FB00..=U+1FB3B run, so those come from other blocks.
fn sextant_char(bits: u8) -> char {
    match bits {
        0 => ' ',
        0b010101 => '\u{258C}', // left half block
        0b101010 => '\u{2590}', // right half block
        0b111111 => '\u{2588}', // full block
        v => {
            let mut index = u32::from(v) - 1;
            if v > 0b010101 {
                index -= 1;
            }
            if v > 0b101010 {
                index -= 1;
            }
            char::from_u32(0x1FB00 + index).expect("sextant codepoints are valid chars")
        }
    }
}

/// Quadrant glyph for a 2x2 bitmask (bit 0 = upper-left, 1 = upper-right,
/// 2 = lower-left, 3 = lower-right). All entries are U+2580..=U+259F or space.
fn quadrant_char(bits: u8) -> char {
    const QUADRANTS: [char; 16] = [
        ' ', '\u{2598}', '\u{259D}', '\u{2580}', '\u{2596}', '\u{258C}', '\u{259E}', '\u{259B}',
        '\u{2597}', '\u{259A}', '\u{2590}', '\u{259C}', '\u{2584}', '\u{2599}', '\u{259F}',
        '\u{2588}',
    ];
    QUADRANTS[usize::from(bits & 0b1111)]
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::Rgb(216, 80, 80);
    const YELLOW: Color = Color::Rgb(196, 196, 12);
    const GREEN: Color = Color::Rgb(136, 185, 125);
    const BASE: Color = Color::Rgb(46, 50, 58);

    fn cell<'a>(med: &'a [Vec<Span<'static>>; 2], x: usize, y: usize) -> &'a Span<'static> {
        &med[y][x]
    }

    /// Eyeball demo — renders the medallion variants as raw true-color ANSI.
    /// Run in a real terminal (this is the design-review artifact for #42):
    ///   cargo test --bin herdr medallion_demo -- --ignored --nocapture
    #[test]
    #[ignore = "visual demo, run with --ignored --nocapture in a real terminal"]
    fn medallion_demo() {
        use std::io::Write;

        let muted = Color::Rgb(110, 116, 128);
        let plain_bg = Color::Rgb(46, 50, 58); // band chrome #2e323a
        let highlight_bg = Color::Rgb(40, 40, 40); // surface_dim row fill

        fn sgr(color: Color, layer: u8) -> String {
            match color {
                Color::Rgb(r, g, b) => format!("\x1b[{layer};2;{r};{g};{b}m"),
                _ => String::new(),
            }
        }
        fn span_ansi(span: &Span<'_>) -> String {
            let mut out = String::new();
            if let Some(fg) = span.style.fg {
                out.push_str(&sgr(fg, 38));
            }
            if let Some(bg) = span.style.bg {
                out.push_str(&sgr(bg, 48));
            }
            out.push_str(&span.content);
            out.push_str("\x1b[0m");
            out
        }

        let combos: [(&str, Vec<Color>); 8] = [
            ("r.y.g", vec![RED, YELLOW, GREEN]),
            ("y.g.g", vec![YELLOW, GREEN, GREEN]),
            ("r.r.r", vec![RED, RED, RED]),
            ("y.y.y", vec![YELLOW, YELLOW, YELLOW]),
            ("r.g.g", vec![RED, GREEN, GREEN]),
            ("r.y  ", vec![RED, YELLOW]),
            ("g    ", vec![GREEN]),
            ("none ", vec![]),
        ];

        let mut out = std::io::stdout();
        for (style_name, style) in [
            ("SEXTANT ", MedallionStyle::Sextant),
            ("QUADRANT", MedallionStyle::Quadrant),
        ] {
            for (bg_name, bg) in [("plain", plain_bg), ("highlight", highlight_bg)] {
                writeln!(out, "\n{style_name} on {bg_name} bg:").unwrap();
                let rendered: Vec<[Vec<Span<'static>>; 2]> = combos
                    .iter()
                    .map(|(_, rings)| ring_medallion(rings, bg, style))
                    .collect();
                let mut label_row = String::from("  ");
                for (label, _) in &combos {
                    label_row.push_str(&format!("{label}   "));
                }
                writeln!(out, "{label_row}").unwrap();
                for line_idx in 0..2 {
                    let mut row = String::from("  ");
                    for medallion in &rendered {
                        for span in &medallion[line_idx] {
                            row.push_str(&span_ansi(span));
                        }
                        row.push_str(&format!("{}     \x1b[0m", sgr(bg, 48)));
                    }
                    writeln!(out, "{row}").unwrap();
                }
            }
        }

        // Alternates for comparison: packed rects + hollow, leading counts.
        writeln!(
            out,
            "\nALTERNATES (packed rects / hollow no-conn / leading counts):"
        )
        .unwrap();
        let rect = |c: Color| format!("{}\u{25AE}\x1b[0m", sgr(c, 38));
        writeln!(
            out,
            "  {}{}{}  {}{}{}  {}\u{25AF}\x1b[0m(no conn)   {}0\x1b[0m {}2\x1b[0m {}1\x1b[0m herdr",
            rect(RED),
            rect(YELLOW),
            rect(GREEN),
            rect(YELLOW),
            rect(GREEN),
            rect(GREEN),
            sgr(muted, 38),
            sgr(muted, 38),
            sgr(YELLOW, 38),
            sgr(GREEN, 38),
        )
        .unwrap();
        writeln!(out).unwrap();
    }

    #[test]
    fn corners_belong_to_the_outer_band() {
        // Rectangular spec: corner cells render middle-band bits as fg over
        // the OUTER color as bg — the corner sub-pixels show rings[0].
        let med = ring_medallion(&[RED, YELLOW, GREEN], BASE, MedallionStyle::Sextant);
        for (x, y) in [(0, 0), (2, 0), (0, 1), (2, 1)] {
            let span = cell(&med, x, y);
            assert_eq!(span.style.bg, Some(RED), "corner cell ({x},{y}) bg");
            assert_eq!(span.style.fg, Some(YELLOW), "corner cell ({x},{y}) fg");
        }
    }

    #[test]
    fn center_column_elects_core_over_outer_bg() {
        // The vertical-center cells cross three bands; innermost wins fg and
        // the bg keeps the perimeter color (middle breaks there by design).
        let med = ring_medallion(&[RED, YELLOW, GREEN], BASE, MedallionStyle::Sextant);
        for y in 0..2 {
            let span = cell(&med, 1, y);
            assert_eq!(span.style.fg, Some(GREEN), "center cell row {y} fg = core");
            assert_eq!(span.style.bg, Some(RED), "center cell row {y} bg = outer");
        }
    }

    #[test]
    fn two_rings_render_border_plus_filled_inner() {
        let med = ring_medallion(&[RED, YELLOW], BASE, MedallionStyle::Sextant);
        // Corner cell: inner fill as fg over outer border bg.
        let corner = cell(&med, 0, 0);
        assert_eq!(corner.style.bg, Some(RED));
        assert_eq!(corner.style.fg, Some(YELLOW));
        // Center cells: middle+core merge into the inner color.
        let center = cell(&med, 1, 0);
        assert_eq!(center.style.fg, Some(YELLOW));
        assert_eq!(center.style.bg, Some(RED));
    }

    #[test]
    fn single_ring_renders_a_solid_rectangle() {
        let med = ring_medallion(&[GREEN], BASE, MedallionStyle::Sextant);
        for line in &med {
            assert_eq!(line.len(), MEDALLION_WIDTH as usize);
            for span in line {
                assert_eq!(span.content.as_ref(), "\u{2588}");
                assert_eq!(span.style.fg, Some(GREEN));
            }
        }
    }

    #[test]
    fn duplicate_ring_colors_merge_into_a_solid_rectangle() {
        let med = ring_medallion(&[RED, RED, RED], BASE, MedallionStyle::Sextant);
        for line in &med {
            for span in line {
                assert_eq!(span.content.as_ref(), "\u{2588}");
                assert_eq!(span.style.fg, Some(RED));
            }
        }
    }

    #[test]
    fn empty_rings_render_an_all_base_blank() {
        let med = ring_medallion(&[], BASE, MedallionStyle::Sextant);
        for line in &med {
            assert_eq!(line.len(), MEDALLION_WIDTH as usize);
            for span in line {
                assert_eq!(span.content.as_ref(), " ");
                assert_eq!(span.style.bg, Some(BASE));
            }
        }
    }

    #[test]
    fn extra_inner_rings_are_ignored() {
        let three = ring_medallion(&[RED, YELLOW, GREEN], BASE, MedallionStyle::Sextant);
        let four = ring_medallion(
            &[RED, YELLOW, GREEN, Color::Blue],
            BASE,
            MedallionStyle::Sextant,
        );
        assert_eq!(three, four);
    }

    #[test]
    fn same_input_yields_same_spans() {
        let a = ring_medallion(&[RED, YELLOW, GREEN], BASE, MedallionStyle::Quadrant);
        let b = ring_medallion(&[RED, YELLOW, GREEN], BASE, MedallionStyle::Quadrant);
        assert_eq!(a, b);
    }

    #[test]
    fn sextant_mode_emits_only_block_mosaic_chars() {
        let med = ring_medallion(&[RED, YELLOW, GREEN], BASE, MedallionStyle::Sextant);
        for line in &med {
            for span in line {
                let ch = span.content.chars().next().unwrap();
                let cp = ch as u32;
                assert!(
                    (0x1FB00..=0x1FB3B).contains(&cp)
                        || ch == ' '
                        || ('\u{2580}'..='\u{259F}').contains(&ch),
                    "unexpected sextant-mode char {ch:?}"
                );
            }
        }
    }

    #[test]
    fn quadrant_mode_emits_only_quadrant_block_chars() {
        let med = ring_medallion(&[RED, YELLOW, GREEN], BASE, MedallionStyle::Quadrant);
        for line in &med {
            for span in line {
                let ch = span.content.chars().next().unwrap();
                assert!(
                    ch == ' ' || ('\u{2580}'..='\u{259F}').contains(&ch),
                    "unexpected quadrant-mode char {ch:?}"
                );
            }
        }
    }

    #[test]
    fn band_geometry_matches_the_documented_raster() {
        // Sextant 6x6 spot checks: nested rectangles, widths 1/1/2.
        let s = MedallionStyle::Sextant;
        assert_eq!(band_index(s, 0, 0), 0, "corner is outer");
        assert_eq!(band_index(s, 5, 0), 0);
        assert_eq!(band_index(s, 3, 0), 0, "top edge is outer");
        assert_eq!(band_index(s, 1, 1), 1, "first inset is middle");
        assert_eq!(band_index(s, 4, 4), 1);
        assert_eq!(band_index(s, 2, 1), 1, "middle band top center");
        assert_eq!(band_index(s, 2, 2), 2, "core 2x2");
        assert_eq!(band_index(s, 3, 3), 2);
        // Quadrant 6x4: vertical-only middle band, core columns 2..=3.
        let q = MedallionStyle::Quadrant;
        assert_eq!(band_index(q, 0, 0), 0);
        assert_eq!(band_index(q, 2, 0), 0, "top edge outer");
        assert_eq!(band_index(q, 1, 1), 1, "side inset middle");
        assert_eq!(band_index(q, 2, 1), 2, "center columns core");
        assert_eq!(band_index(q, 3, 2), 2);
    }
}
