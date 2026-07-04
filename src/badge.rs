//! Tray icon composition: the dolphin plus a framed `running/open` counter,
//! the same design as the macOS status item badge. Pure pixel work on ARGB32
//! buffers — no image or font dependencies; digits come from an embedded 5x7
//! pixel font.

pub const SIZE: usize = 96;

/// Compose the tray icon. The dolphin always stays at its full 96x96 size;
/// open sessions overlay a filled `running/open` counter on the lower-right
/// corner, the way app icons carry notification badges. (A wide side-by-side
/// pixmap does not work here: Plasma scales icons into a square tray cell,
/// which would shrink everything.)
pub fn compose(base: &[u8], running: usize, open: usize, color: (u8, u8, u8)) -> Vec<u8> {
    let mut canvas = base.to_vec();
    if open > 0 {
        draw_badge(&mut canvas, &format!("{running}/{open}"), color);
    }
    canvas
}

fn draw_badge(canvas: &mut [u8], label: &str, color: (u8, u8, u8)) {
    // Lower-right corner, overlaid on the dolphin like an app badge.
    const X0: i32 = 34;
    const Y0: i32 = 52;
    const X1: i32 = 96;
    const Y1: i32 = 96;
    const RADIUS: i32 = 14;
    const STROKE: i32 = 4;
    const FILL: (u8, u8, u8) = (0x20, 0x20, 0x20);

    for y in Y0..Y1 {
        for x in X0..X1 {
            let outer = in_rounded_rect(x, y, X0, Y0, X1, Y1, RADIUS);
            let inner = in_rounded_rect(
                x,
                y,
                X0 + STROKE,
                Y0 + STROKE,
                X1 - STROKE,
                Y1 - STROKE,
                RADIUS - STROKE,
            );
            if inner {
                // Solid dark plate keeps the digits readable over the art.
                set_px(canvas, x as usize, y as usize, FILL);
            } else if outer {
                set_px(canvas, x as usize, y as usize, color);
            }
        }
    }

    // Largest text scale that fits the badge interior.
    let chars: Vec<char> = label.chars().collect();
    let text_width = |s: i32| chars.len() as i32 * 5 * s + (chars.len() as i32 - 1) * s;
    let scale: i32 = (1..=4)
        .rev()
        .find(|s| text_width(*s) <= X1 - X0 - 2 * STROKE - 4)
        .unwrap_or(1);
    let total = text_width(scale);
    let mut cursor_x = X0 + (X1 - X0 - total) / 2;
    let text_y = Y0 + (Y1 - Y0 - 7 * scale) / 2;
    for c in chars {
        draw_glyph(canvas, c, cursor_x, text_y, scale, color);
        cursor_x += 6 * scale;
    }
}

fn in_rounded_rect(x: i32, y: i32, x0: i32, y0: i32, x1: i32, y1: i32, r: i32) -> bool {
    if x < x0 || x >= x1 || y < y0 || y >= y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - 1 - r);
    let cy = y.clamp(y0 + r, y1 - 1 - r);
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy <= r * r
}

fn set_px(canvas: &mut [u8], x: usize, y: usize, color: (u8, u8, u8)) {
    if x >= SIZE || y >= SIZE {
        return;
    }
    let idx = (y * SIZE + x) * 4;
    canvas[idx] = 0xFF; // A (network byte order ARGB)
    canvas[idx + 1] = color.0;
    canvas[idx + 2] = color.1;
    canvas[idx + 3] = color.2;
}

fn draw_glyph(canvas: &mut [u8], c: char, gx: i32, gy: i32, scale: i32, color: (u8, u8, u8)) {
    let Some(rows) = glyph(c) else { return };
    for (row, bits) in rows.iter().enumerate() {
        for col in 0..5 {
            if bits & (0b10000 >> col) != 0 {
                for sy in 0..scale {
                    for sx in 0..scale {
                        let x = gx + col * scale + sx;
                        let y = gy + row as i32 * scale + sy;
                        if x >= 0 && y >= 0 {
                            set_px(canvas, x as usize, y as usize, color);
                        }
                    }
                }
            }
        }
    }
}

/// 5x7 pixel font, digits and the slash — everything a `running/open` badge
/// needs.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        '/' => [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<u8> {
        vec![0x80; SIZE * SIZE * 4]
    }

    fn colored_pixels(buffer: &[u8], color: (u8, u8, u8)) -> usize {
        buffer
            .chunks_exact(4)
            .filter(|p| p[0] == 0xFF && p[1] == color.0 && p[2] == color.1 && p[3] == color.2)
            .count()
    }

    #[test]
    fn no_open_sessions_returns_plain_base() {
        assert_eq!(compose(&base(), 0, 0, (1, 2, 3)), base());
    }

    #[test]
    fn badge_overlays_corner_and_leaves_top_untouched() {
        let color = (0xE8, 0xE8, 0xE8);
        let icon = compose(&base(), 1, 2, color);
        assert_eq!(icon.len(), SIZE * SIZE * 4);
        // The top half of the dolphin stays untouched.
        assert_eq!(&icon[..SIZE * 4 * 48], &base()[..SIZE * 4 * 48]);
        // Frame + "1/2" should paint a substantial number of pixels.
        assert!(colored_pixels(&icon, color) > 300);
    }

    #[test]
    fn wide_labels_still_fit() {
        let color = (0xFF, 0x9F, 0x0A);
        let icon = compose(&base(), 12, 34, color);
        // "12/34" at a smaller scale still renders text pixels.
        assert!(colored_pixels(&icon, color) > 300);
    }

    #[test]
    fn all_needed_glyphs_exist() {
        for c in "0123456789/".chars() {
            assert!(glyph(c).is_some(), "missing glyph {c}");
        }
        assert!(glyph('x').is_none());
    }
}
