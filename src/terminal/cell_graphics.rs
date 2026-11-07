use gpui_kit::{Bounds, Hsla, Pixels, Window, fill, point, px};

/// Solid blocks and vertical border strokes must reach their cell edges regardless
/// of the font's glyph metrics or the space between text baselines.
pub(super) fn paint(window: &mut Window, text: &str, cell: Bounds<Pixels>, color: Hsla) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(character), None) => shapes(character, cell, |bounds| {
            window.paint_quad(fill(bounds, color));
        }),
        _ => false,
    }
}

fn shapes(character: char, cell: Bounds<Pixels>, mut draw: impl FnMut(Bounds<Pixels>)) -> bool {
    let mut block = |left, top, right, bottom| {
        let x = |eighth: u8| cell.left() + cell.size.width * (f32::from(eighth) / 8.);
        let y = |eighth: u8| cell.top() + cell.size.height * (f32::from(eighth) / 8.);
        draw(Bounds::from_corners(
            point(x(left), y(top)),
            point(x(right), y(bottom)),
        ));
    };

    match character {
        '▀' => block(0, 0, 8, 4),
        '▁'..='█' => block(0, ('█' as u32 - character as u32) as u8, 8, 8),
        '▉'..='▏' => block(0, 0, ('▐' as u32 - character as u32) as u8, 8),
        '▐' => block(4, 0, 8, 8),
        '▔' => block(0, 0, 8, 1),
        '▕' => block(7, 0, 8, 8),
        '▖'..='▟' => {
            // Bits follow the quadrants from top left to bottom right.
            let mask = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14][character as usize - '▖' as usize];
            for (index, (x, y)) in [(0, 0), (4, 0), (0, 4), (4, 4)].into_iter().enumerate() {
                if mask & (1 << index) != 0 {
                    block(x, y, x + 4, y + 4);
                }
            }
        }
        _ => return heavy_vertical(character, cell, draw),
    }
    true
}

fn heavy_vertical(
    character: char,
    cell: Bounds<Pixels>,
    mut draw: impl FnMut(Bounds<Pixels>),
) -> bool {
    let center = cell.center();
    let (top, bottom) = match character {
        '┃' => (cell.top(), cell.bottom()),
        '╹' => (cell.top(), center.y),
        '╻' => (center.y, cell.bottom()),
        _ => return false,
    };
    let half_width = px(1.).min(cell.size.width / 2.);
    draw(Bounds::from_corners(
        point(center.x - half_width, top),
        point(center.x + half_width, bottom),
    ));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(row: f32) -> Bounds<Pixels> {
        Bounds::from_corners(point(px(17.5), px(row)), point(px(25.25), px(row + 22.)))
    }

    fn bounds(character: char, row: f32) -> Bounds<Pixels> {
        let mut result = None;
        assert!(shapes(character, cell(row), |bounds| {
            assert!(result.replace(bounds).is_none());
        }));
        result.unwrap()
    }

    #[test]
    fn neighboring_half_blocks_share_the_row_edge() {
        assert_eq!(bounds('▄', 0.).bottom(), bounds('▀', 22.).top());
        assert_eq!(bounds('█', 0.), cell(0.));
        assert_eq!(bounds('▎', 0.).bottom(), bounds('▎', 22.).top());
    }

    #[test]
    fn heavy_vertical_stroke_joins_its_half_cell_cap() {
        let line = bounds('┃', 0.);
        let cap = bounds('╹', 22.);
        assert_eq!(line.bottom(), cap.top());
        assert_eq!(line.left(), cap.left());
        assert_eq!(line.right(), cap.right());
        assert_eq!(cap.bottom(), cell(22.).center().y);
    }
}
