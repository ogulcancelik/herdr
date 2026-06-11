//! Ring medallion: concentric status rings rasterized into sub-cell block
//! characters for the sidebar's two-line server rows (issue #42).
//!
//! The medallion occupies a 2-cell-wide x 2-line-tall block. Sub-cell
//! resolution comes from block-mosaic glyphs:
//!
//! * [`MedallionStyle::Sextant`] — 2x3 sub-blocks per cell (Unicode "Symbols
//!   for Legacy Computing" sextants, U+1FB00..=U+1FB3B) for a 4x6 grid.
//! * [`MedallionStyle::Quadrant`] — 2x2 sub-blocks per cell (quadrant blocks,
//!   U+2580..=U+259F) for a 4x4 grid. Safer font coverage; the caller picks.
//!
//! Ideal raster (what the rings *mean*), `.` = `base_bg`, `o` = `rings[0]`
//! (outer), `m` = middle, `c` = core (innermost):
//!
//! ```text
//!   Sextant (4x6)        Quadrant (4x4)
//!     . o o .               . o o .
//!     o m m o               o c c o
//!     o c c o               o c c o
//!     o c c o               . o o .
//!     o m m o
//!     o o o .  <- (corners are always base_bg: rounded shape)
//! ```
//!
//! ## The approximation (why this is not a true bullseye)
//!
//! A terminal cell carries exactly one fg + one bg. Every cell of the 2x2
//! block touches a grid corner (which must stay `base_bg` so the medallion
//! composes with row highlight/selection fills), the outer ring, *and* the
//! core — three colors, two slots. A faithful multi-color bullseye is
//! therefore not expressible. Resolution:
//!
//! * bg is always `base_bg` (the compose guarantee wins over ring fidelity);
//!   each cell's glyph leaves its grid-corner sub-block unlit so the corner
//!   shows the row fill.
//! * fg: each cell elects one ring color; elections walk the severity-sorted
//!   ring list along the TL -> TR/BL -> BR diagonal, so the outermost (worst)
//!   color anchors the top-left and the innermost color the bottom-right.
//!   All ring sub-blocks in a cell collapse to that single fg.
//!
//! ```text
//!   elected fg per cell:
//!   3 rings        2 rings        1 ring
//!   [r0][r1]       [r0][r0]       [r0][r0]
//!   [r1][r2]       [r1][r1]       [r0][r0]
//! ```
//!
//! Rendered glyphs (fg = elected ring color, bg = `base_bg`):
//!
//! ```text
//!   Sextant:   🬻🬺      Quadrant:   ▟▙
//!              🬬🬝                  ▜▛
//! ```
//!
//! Edge cases: one ring renders a solid rounded dot in a `base_bg` field;
//! empty rings render an all-`base_bg` blank; duplicate colors stay
//! well-formed. At most three rings are shown; extra (inner) entries are
//! ignored.

use ratatui::style::{Color, Style};
use ratatui::text::Span;

/// Medallion width in terminal cells (per line; the medallion is two lines
/// tall).
pub(crate) const MEDALLION_WIDTH: u16 = 2;

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

/// Sub-pixel classification on the medallion grid, outer to inner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pixel {
    /// Outside the outer ring: painted `base_bg` so the medallion composes
    /// with row highlight/selection fills.
    Base,
    /// Outer ring: the grid border minus its corners.
    Outer,
    /// Middle ring: the next inset band (sextant grid only; the 4x4 quadrant
    /// grid has no room for it).
    Middle,
    /// Core: the 2x2 sub-block middle of the grid.
    Core,
}

/// Renders the medallion for `rings` (ordered OUTER -> INNER, severity-sorted
/// by the caller, at most three used) as two lines of two 1-cell spans each.
///
/// `base_bg` paints every sub-region outside the outer ring and is the bg of
/// every cell, so the medallion composes with row highlight/selection fills.
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

    let mut lines = [Vec::with_capacity(2), Vec::with_capacity(2)];
    for (cell_y, line) in lines.iter_mut().enumerate() {
        for cell_x in 0..MEDALLION_WIDTH as usize {
            let rank = cell_y * MEDALLION_WIDTH as usize + cell_x;
            let fg = rings[elected_ring(rank, rings.len())];
            let bits = cell_bits(style, cell_x, cell_y);
            let glyph = match style {
                MedallionStyle::Sextant => sextant_char(bits),
                MedallionStyle::Quadrant => quadrant_char(bits),
            };
            line.push(Span::styled(
                glyph.to_string(),
                Style::default().fg(fg).bg(base_bg),
            ));
        }
    }
    lines
}

/// Which ring color a cell elects as its fg: walk the ring list (outer ->
/// inner) along the cell diagonal TL(0) -> TR(1)/BL(2) -> BR(3), rounding to
/// the nearest ring. See the module docs for why one fg per cell forces this.
fn elected_ring(cell_rank: usize, ring_count: usize) -> usize {
    debug_assert!((1..=3).contains(&ring_count) && cell_rank < 4);
    // round(cell_rank * (ring_count - 1) / 3) in integer arithmetic.
    (cell_rank * (ring_count - 1) * 2 + 3) / 6
}

/// (columns, rows) of sub-blocks within one terminal cell.
fn cell_sub_dims(style: MedallionStyle) -> (usize, usize) {
    match style {
        MedallionStyle::Sextant => (2, 3),
        MedallionStyle::Quadrant => (2, 2),
    }
}

/// Classifies one sub-pixel of the full medallion grid (4x6 or 4x4).
fn classify(style: MedallionStyle, x: usize, y: usize) -> Pixel {
    let (sub_cols, sub_rows) = cell_sub_dims(style);
    let (cols, rows) = (
        sub_cols * MEDALLION_WIDTH as usize,
        sub_rows * MEDALLION_WIDTH as usize,
    );
    let edge_x = x == 0 || x == cols - 1;
    let edge_y = y == 0 || y == rows - 1;
    if edge_x && edge_y {
        return Pixel::Base; // rounded corner
    }
    if edge_x || edge_y {
        return Pixel::Outer; // border minus corners
    }
    match style {
        // interior rows of the 4x6 grid: 1 & 4 are the middle band, 2 & 3 the
        // core.
        MedallionStyle::Sextant if y == 1 || y == rows - 2 => Pixel::Middle,
        // the 4x4 grid's whole interior is core (no room for a middle band).
        _ => Pixel::Core,
    }
}

/// Lit-sub-block bitmask for one cell of the medallion: bit `sy * 2 + sx` is
/// set when the sub-pixel belongs to any ring (everything except `Base`).
fn cell_bits(style: MedallionStyle, cell_x: usize, cell_y: usize) -> u8 {
    let (sub_cols, sub_rows) = cell_sub_dims(style);
    let mut bits = 0u8;
    for sy in 0..sub_rows {
        for sx in 0..sub_cols {
            if classify(style, cell_x * sub_cols + sx, cell_y * sub_rows + sy) != Pixel::Base {
                bits |= 1 << (sy * sub_cols + sx);
            }
        }
    }
    bits
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
    /// Eyeball demo — renders the medallion variants as raw true-color ANSI.
    /// Run in a real terminal (this is the design-review artifact for #42):
    ///   cargo test --bin herdr medallion_demo -- --ignored --nocapture
    #[test]
    #[ignore = "visual demo, run with --ignored --nocapture in a real terminal"]
    fn medallion_demo() {
        use std::io::Write;

        // dalton palette
        let red = Color::Rgb(216, 80, 80);
        let yellow = Color::Rgb(196, 196, 12);
        let green = Color::Rgb(136, 185, 125);
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
            ("r.y.g", vec![red, yellow, green]),
            ("y.g.g", vec![yellow, green, green]),
            ("r.r.r", vec![red, red, red]),
            ("y.y.y", vec![yellow, yellow, yellow]),
            ("r.g.g", vec![red, green, green]),
            ("r.y  ", vec![red, yellow]),
            ("g    ", vec![green]),
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
                        row.push_str(&format!("{}      \x1b[0m", sgr(bg, 48)));
                    }
                    writeln!(out, "{row}").unwrap();
                }
            }
        }

        // Alternates for comparison: packed rects + hollow, circle + digits.
        writeln!(
            out,
            "\nALTERNATES (packed rects / hollow no-conn / circle+digits):"
        )
        .unwrap();
        let rect = |c: Color| format!("{}\u{25AE}\x1b[0m", sgr(c, 38));
        writeln!(
            out,
            "  {}{}{}  {}{}{}  {}\u{25AF}\x1b[0m(no conn)   {}\u{25CF}\x1b[0m herdr {}2\x1b[0m {}1\x1b[0m",
            rect(red), rect(yellow), rect(green),
            rect(yellow), rect(green), rect(green),
            sgr(muted, 38),
            sgr(red, 38),
            sgr(red, 38),
            sgr(yellow, 38),
        )
        .unwrap();
        writeln!(out).unwrap();
    }

    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;

    const BASE: Color = Color::Rgb(20, 22, 30);
    const OUTER: Color = Color::Red;
    const MIDDLE: Color = Color::Yellow;
    const INNER: Color = Color::Green;

    fn render(lines: &[Vec<Span<'static>>; 2]) -> Buffer {
        let backend = TestBackend::new(2, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let paragraph = Paragraph::new(vec![
                    Line::from(lines[0].clone()),
                    Line::from(lines[1].clone()),
                ]);
                frame.render_widget(paragraph, Rect::new(0, 0, 2, 2));
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn cells(buffer: &Buffer) -> Vec<(char, Option<Color>, Option<Color>)> {
        [(0, 0), (1, 0), (0, 1), (1, 1)]
            .into_iter()
            .map(|(x, y)| {
                let cell = &buffer[(x, y)];
                (
                    cell.symbol().chars().next().unwrap(),
                    cell.style().fg,
                    cell.style().bg,
                )
            })
            .collect()
    }

    /// Glyphs that light every sub-block except the one facing the medallion
    /// grid corner, per cell TL/TR/BL/BR.
    const SEXTANT_GLYPHS: [char; 4] = ['\u{1FB3B}', '\u{1FB3A}', '\u{1FB2C}', '\u{1FB1D}'];
    const QUADRANT_GLYPHS: [char; 4] = ['\u{259F}', '\u{2599}', '\u{259C}', '\u{259B}'];

    #[test]
    fn corner_cells_compose_with_base_bg() {
        for (style, glyphs) in [
            (MedallionStyle::Sextant, SEXTANT_GLYPHS),
            (MedallionStyle::Quadrant, QUADRANT_GLYPHS),
        ] {
            let lines = ring_medallion(&[OUTER, MIDDLE, INNER], BASE, style);
            let buffer = render(&lines);
            for (i, (ch, _, bg)) in cells(&buffer).into_iter().enumerate() {
                // every cell's glyph leaves its grid-corner sub-block as bg...
                assert_eq!(ch, glyphs[i], "style {style:?} cell {i}");
                // ...and that bg is base_bg, so row highlight fills show
                // through the rounded corners.
                assert_eq!(bg, Some(BASE), "style {style:?} cell {i}");
            }
        }
    }

    #[test]
    fn outer_ring_color_appears_on_edge_cells() {
        for style in [MedallionStyle::Sextant, MedallionStyle::Quadrant] {
            for rings in [
                &[OUTER][..],
                &[OUTER, INNER][..],
                &[OUTER, MIDDLE, INNER][..],
            ] {
                let buffer = render(&ring_medallion(rings, BASE, style));
                let top_left = &buffer[(0, 0)];
                assert_eq!(top_left.style().fg, Some(OUTER), "style {style:?}");
            }
        }
    }

    #[test]
    fn center_cells_carry_the_innermost_color() {
        for style in [MedallionStyle::Sextant, MedallionStyle::Quadrant] {
            for rings in [
                &[INNER][..],
                &[OUTER, INNER][..],
                &[OUTER, MIDDLE, INNER][..],
            ] {
                let buffer = render(&ring_medallion(rings, BASE, style));
                // BR is the cell that reaches the core's lower-right quarter.
                let bottom_right = &buffer[(1, 1)];
                assert_eq!(bottom_right.style().fg, Some(INNER), "style {style:?}");
            }
        }
    }

    #[test]
    fn ring_elections_walk_the_diagonal() {
        assert_eq!(
            (0..4).map(|r| elected_ring(r, 1)).collect::<Vec<_>>(),
            [0, 0, 0, 0]
        );
        assert_eq!(
            (0..4).map(|r| elected_ring(r, 2)).collect::<Vec<_>>(),
            [0, 0, 1, 1]
        );
        assert_eq!(
            (0..4).map(|r| elected_ring(r, 3)).collect::<Vec<_>>(),
            [0, 1, 1, 2]
        );
    }

    #[test]
    fn quadrant_mode_emits_only_quadrant_block_chars() {
        for rings in [
            &[][..],
            &[OUTER][..],
            &[OUTER, INNER][..],
            &[OUTER, MIDDLE, INNER][..],
        ] {
            let buffer = render(&ring_medallion(rings, BASE, MedallionStyle::Quadrant));
            for (ch, _, _) in cells(&buffer) {
                assert!(
                    (0x2580..=0x259F).contains(&(ch as u32)) || ch == ' ',
                    "unexpected quadrant glyph {ch:?}"
                );
            }
        }
    }

    #[test]
    fn sextant_mode_emits_only_legacy_computing_chars() {
        for rings in [
            &[][..],
            &[OUTER][..],
            &[OUTER, INNER][..],
            &[OUTER, MIDDLE, INNER][..],
        ] {
            let buffer = render(&ring_medallion(rings, BASE, MedallionStyle::Sextant));
            for (ch, _, _) in cells(&buffer) {
                assert!(
                    (0x1FB00..=0x1FB3B).contains(&(ch as u32)) || ch == ' ' || ch == '\u{2588}',
                    "unexpected sextant glyph {ch:?}"
                );
            }
        }
    }

    #[test]
    fn single_ring_renders_a_solid_dot_in_a_base_field() {
        for style in [MedallionStyle::Sextant, MedallionStyle::Quadrant] {
            let buffer = render(&ring_medallion(&[OUTER], BASE, style));
            for (_, fg, bg) in cells(&buffer) {
                assert_eq!(fg, Some(OUTER));
                assert_eq!(bg, Some(BASE));
            }
        }
    }

    #[test]
    fn empty_rings_render_an_all_base_blank() {
        for style in [MedallionStyle::Sextant, MedallionStyle::Quadrant] {
            let buffer = render(&ring_medallion(&[], BASE, style));
            for (ch, _, bg) in cells(&buffer) {
                assert_eq!(ch, ' ');
                assert_eq!(bg, Some(BASE));
            }
        }
    }

    #[test]
    fn duplicate_ring_colors_stay_well_formed() {
        for style in [MedallionStyle::Sextant, MedallionStyle::Quadrant] {
            let lines = ring_medallion(&[OUTER, OUTER, OUTER], BASE, style);
            assert_eq!(lines[0].len(), 2);
            assert_eq!(lines[1].len(), 2);
            for (_, fg, bg) in cells(&render(&lines)) {
                assert_eq!(fg, Some(OUTER));
                assert_eq!(bg, Some(BASE));
            }
        }
    }

    #[test]
    fn extra_inner_rings_are_ignored() {
        for style in [MedallionStyle::Sextant, MedallionStyle::Quadrant] {
            assert_eq!(
                ring_medallion(&[OUTER, MIDDLE, INNER, Color::Blue], BASE, style),
                ring_medallion(&[OUTER, MIDDLE, INNER], BASE, style),
            );
        }
    }

    #[test]
    fn same_input_yields_same_spans() {
        for style in [MedallionStyle::Sextant, MedallionStyle::Quadrant] {
            for rings in [
                &[][..],
                &[OUTER][..],
                &[OUTER, INNER][..],
                &[OUTER, MIDDLE, INNER][..],
            ] {
                assert_eq!(
                    ring_medallion(rings, BASE, style),
                    ring_medallion(rings, BASE, style),
                );
            }
        }
    }

    #[test]
    fn grid_geometry_matches_the_documented_raster() {
        // sextant 4x6: rounded corners, bordering outer ring, middle band on
        // interior rows 1 & 4, core on rows 2 & 3.
        let s = MedallionStyle::Sextant;
        for (x, y) in [(0, 0), (3, 0), (0, 5), (3, 5)] {
            assert_eq!(classify(s, x, y), Pixel::Base);
        }
        for (x, y) in [(1, 0), (2, 0), (0, 1), (3, 2), (0, 4), (1, 5)] {
            assert_eq!(classify(s, x, y), Pixel::Outer);
        }
        for (x, y) in [(1, 1), (2, 1), (1, 4), (2, 4)] {
            assert_eq!(classify(s, x, y), Pixel::Middle);
        }
        for (x, y) in [(1, 2), (2, 2), (1, 3), (2, 3)] {
            assert_eq!(classify(s, x, y), Pixel::Core);
        }
        // quadrant 4x4: rounded corners, outer border, all-core interior.
        let q = MedallionStyle::Quadrant;
        for (x, y) in [(0, 0), (3, 0), (0, 3), (3, 3)] {
            assert_eq!(classify(q, x, y), Pixel::Base);
        }
        for (x, y) in [(1, 0), (0, 2), (3, 1), (2, 3)] {
            assert_eq!(classify(q, x, y), Pixel::Outer);
        }
        for (x, y) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            assert_eq!(classify(q, x, y), Pixel::Core);
        }
    }
}
