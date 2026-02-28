// Cursor sprite — 14 × 20 pixels, hot spot at (0, 0)
//
// Each row is a u16 with bit 15 = col 0 (leftmost pixel).
//
// CURSOR_MASK[row]:  bit = 1 → opaque pixel (draw something here)
// CURSOR_IMAGE[row]: bit = 1 → white fill, bit = 0 → black outline
//                   (only meaningful where CURSOR_MASK = 1)
//
// Visual (■ = black outline, □ = white fill, · = transparent):
//
//   col  0  1  2  3  4  5  6  7  8  9 10 11 12 13
//   row 0:  ■  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
//   row 1:  ■  ■  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
//   row 2:  ■  □  ■  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
//   row 3:  ■  □  □  ■  ·  ·  ·  ·  ·  ·  ·  ·  ·  ·
//   row 4:  ■  □  □  □  ■  ·  ·  ·  ·  ·  ·  ·  ·  ·
//   row 5:  ■  □  □  □  □  ■  ·  ·  ·  ·  ·  ·  ·  ·
//   row 6:  ■  □  □  □  □  □  ■  ·  ·  ·  ·  ·  ·  ·
//   row 7:  ■  □  □  □  □  □  □  ■  ·  ·  ·  ·  ·  ·
//   row 8:  ■  □  □  □  □  □  □  □  ■  ·  ·  ·  ·  ·
//   row 9:  ■  □  □  □  □  □  □  □  □  ■  ·  ·  ·  ·
//   row10:  ■  □  □  □  □  □  □  □  □  □  ■  ·  ·  ·
//   row11:  ■  □  □  □  □  □  □  □  □  □  □  ■  ·  ·
//   row12:  ■  □  □  □  □  □  □  □  □  □  □  □  ■  ·
//   row13:  ■  □  □  □  □  □  □  ■  ■  ■  ■  ■  ■  ■
//   row14:  ■  □  □  □  ■  □  □  ■  ·  ·  ·  ·  ·  ·
//   row15:  ■  □  □  ■  ·  ■  □  □  ■  ·  ·  ·  ·  ·
//   row16:  ■  □  ■  ·  ·  ■  □  □  ■  ·  ·  ·  ·  ·
//   row17:  ■  ■  ·  ·  ·  ·  ■  □  □  ■  ·  ·  ·  ·
//   row18:  ·  ·  ·  ·  ·  ·  ■  □  □  ■  ·  ·  ·  ·
//   row19:  ·  ·  ·  ·  ·  ·  ·  ■  ■  ·  ·  ·  ·  ·

pub const CURSOR_W: u32 = 14;
pub const CURSOR_H: u32 = 20;

#[rustfmt::skip]
pub const CURSOR_MASK: [u16; 20] = [
    0x8000, // row  0: ■
    0xC000, // row  1: ■ ■
    0xE000, // row  2: ■ □ ■
    0xF000, // row  3: ■ □ □ ■
    0xF800, // row  4: ■ □ □ □ ■
    0xFC00, // row  5: ■ □ □ □ □ ■
    0xFE00, // row  6: ■ □ □ □ □ □ ■
    0xFF00, // row  7: ■ □ □ □ □ □ □ ■
    0xFF80, // row  8: ■ □ □ □ □ □ □ □ ■
    0xFFC0, // row  9: ■ □ □ □ □ □ □ □ □ ■
    0xFFE0, // row 10: ■ □ □ □ □ □ □ □ □ □ ■
    0xFFF0, // row 11: ■ □ □ □ □ □ □ □ □ □ □ ■
    0xFFF8, // row 12: ■ □ □ □ □ □ □ □ □ □ □ □ ■
    0xFFFC, // row 13: ■ □ □ □ □ □ □ ■ ■ ■ ■ ■ ■ ■
    0xFF00, // row 14: ■ □ □ □ ■ □ □ ■
    0xF780, // row 15: ■ □ □ ■ · ■ □ □ ■
    0xE780, // row 16: ■ □ ■ · · ■ □ □ ■
    0xC3C0, // row 17: ■ ■ · · · · ■ □ □ ■
    0x03C0, // row 18: · · · · · · ■ □ □ ■
    0x0180, // row 19: · · · · · · · ■ ■ ·
];

#[rustfmt::skip]
pub const CURSOR_IMAGE: [u16; 20] = [
    0x0000, // row  0: black
    0x0000, // row  1: black
    0x4000, // row  2: . □ .
    0x6000, // row  3: . □ □ .
    0x7000, // row  4: . □ □ □ .
    0x7800, // row  5: . □ □ □ □ .
    0x7C00, // row  6: . □ □ □ □ □ .
    0x7E00, // row  7: . □ □ □ □ □ □ .
    0x7F00, // row  8: . □ □ □ □ □ □ □ .
    0x7F80, // row  9: . □ □ □ □ □ □ □ □ .
    0x7FC0, // row 10: . □ □ □ □ □ □ □ □ □ .
    0x7FE0, // row 11: . □ □ □ □ □ □ □ □ □ □ .
    0x7FF0, // row 12: . □ □ □ □ □ □ □ □ □ □ □ .
    0x7E00, // row 13: . □ □ □ □ □ □ . . . . . . .
    0x7600, // row 14: . □ □ □ . □ □ .
    0x6300, // row 15: . □ □ . . . □ □ .
    0x4300, // row 16: . □ . . . . □ □ .
    0x0180, // row 17: . . . . . . . □ □ .
    0x0180, // row 18: . . . . . . . □ □ .
    0x0000, // row 19: black end
];
