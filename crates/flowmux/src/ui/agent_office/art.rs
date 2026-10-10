// SPDX-License-Identifier: GPL-3.0-or-later
//! Hand-authored office furnishings. All shapes are drawn locally in world coordinates.

use super::{
    layout::{Plan, Rect, OBJECT_SCALE},
    scene::rect,
};
use gtk::cairo::Context;

pub(super) struct Palette {
    pub wall: u32,
    pub floor: u32,
    pub seam: u32,
    pub rug: u32,
    pub accent: u32,
    pub wood: u32,
    pub seat: u32,
}
pub(super) const PALETTES: [Palette; 6] = [
    Palette {
        wall: 0x526760,
        floor: 0xc9b493,
        seam: 0xbea889,
        rug: 0x8c9c91,
        accent: 0xf1cf8d,
        wood: 0xceaa79,
        seat: 0xb57053,
    },
    Palette {
        wall: 0x506956,
        floor: 0xc2bda1,
        seam: 0xb3af94,
        rug: 0x93a18a,
        accent: 0xd7df9b,
        wood: 0xb5a875,
        seat: 0xe1b685,
    },
    Palette {
        wall: 0x263e59,
        floor: 0xa9b6b6,
        seam: 0x9daaaa,
        rug: 0x69858d,
        accent: 0x92d8dc,
        wood: 0xadbcc6,
        seat: 0xdea568,
    },
    Palette {
        wall: 0x665c51,
        floor: 0xc2ab8e,
        seam: 0xb69d80,
        rug: 0xa18370,
        accent: 0xe9be98,
        wood: 0xbe956f,
        seat: 0x89aa9e,
    },
    Palette {
        wall: 0x484d4e,
        floor: 0xb4b1a4,
        seam: 0xa8a698,
        rug: 0x87938c,
        accent: 0xf0bc6a,
        wood: 0xc58a56,
        seat: 0x7c9ba2,
    },
    Palette {
        wall: 0x6f7765,
        floor: 0xd0bea2,
        seam: 0xc2b095,
        rug: 0xba977e,
        accent: 0xf0d5aa,
        wood: 0xdbb087,
        seat: 0x89aa88,
    },
];

pub(super) fn shell(cr: &Context, plan: &Plan, design: usize) {
    let theme = design / 4;
    let p = &PALETTES[theme];
    let (w, h) = (plan.width, plan.height);
    rect(cr, 0., 0., w, h, p.floor);
    // Long staggered boards and small mineral chips keep the floor at a quiet scale.
    for row in 0..(h / 20.) as usize + 1 {
        let y = row as f64 * 20.;
        for col in 0..(w / 96.) as usize + 1 {
            let x = col as f64 * 96. - (row % 3) as f64 * 32.;
            if matches!(theme, 1 | 2 | 4) {
                rect(cr, x + 17., y + 7., 2., 1., p.seam);
                rect(cr, x + 58., y + 13., 3., 2., p.seam);
            } else {
                rect(cr, x, y, 96., 1., p.seam);
                rect(cr, x, y, 1., 20., p.seam);
                if (row + col) % 3 == 0 {
                    rect(cr, x + 12., y + 8., 27., 1., p.seam);
                    rect(cr, x + 21., y + 11., 11., 1., p.seam);
                }
            }
        }
    }
    // Textiles belong to seating groups; the remaining floor is a shared circulation space.
    for &(x, y) in &plan.benches {
        let _ = cr.save();
        cr.translate(x, y);
        cr.scale(OBJECT_SCALE, OBJECT_SCALE);
        cr.translate(-x, -y);
        rect(cr, x - 49., y - 49., 122., 65., p.rug);
        rect(cr, x - 45., y - 45., 114., 1., p.accent);
        rect(cr, x - 45., y + 11., 114., 1., p.accent);
        for i in 0..19 {
            let xx = x - 44. + i as f64 * 6.;
            rect(cr, xx, y - 51., 2., 3., p.accent);
            rect(cr, xx, y + 16., 2., 3., p.accent);
            rect(cr, xx, y + 4., 2., 3., p.seam);
        }
        let _ = cr.restore();
    }
    // Plaster above stained wood panelling, with a recessed skirting shadow.
    rect(cr, 0., 0., w, 60., 0xe2d8c3);
    rect(cr, 0., 34., w, 24., p.wall);
    for col in 0..(w / 32.) as usize + 1 {
        rect(cr, col as f64 * 32., 36., 2., 20., p.rug);
    }
    rect(cr, 0., 33., w, 3., p.wood);
    rect(cr, 0., 57., w, 4., p.wood);
    let _ = cr.save();
    cr.set_source_rgba(0.20, 0.18, 0.14, 0.10);
    cr.rectangle(0., 61., w, 5.);
    let _ = cr.fill();
    // Window mullions break the light into narrow bands instead of a large flat polygon.
    cr.set_source_rgba(1., 0.96, 0.80, if theme == 2 { 0.04 } else { 0.16 });
    for i in 0..3 {
        let x = w / 2. - 48. + i as f64 * 33.;
        cr.move_to(x, 62.);
        cr.line_to(x + 28., 62.);
        cr.line_to(x + 75., 156.);
        cr.line_to(x + 47., 156.);
        cr.close_path();
        let _ = cr.fill();
    }
    let _ = cr.restore();
    rect(cr, 0., h - 12., w, 12., p.seam);
    rect(cr, 0., h - 12., w, 2., p.wood);
    for x in [0., w - 4.] {
        rect(cr, x, 60., 4., h - 72., p.wood);
    }
    let window_w = (w * 0.35).clamp(64., 180.);
    let x = (w - window_w) / 2.;
    rect(cr, x - 4., 8., window_w + 8., 41., 0x24313b);
    rect(
        cr,
        x,
        11.,
        window_w,
        32.,
        if theme == 2 { 0x29495f } else { 0x91bcc1 },
    );
    for i in 0..(window_w / 16.) as usize {
        let xx = x + i as f64 * 16.;
        if theme == 2 {
            rect(cr, xx + 4., 15. + (i % 3) as f64 * 5., 2., 2., p.accent);
            rect(
                cr,
                xx,
                33. - (i % 4) as f64 * 3.,
                10.,
                10. + (i % 4) as f64 * 3.,
                0x1c354b,
            );
        } else {
            rect(
                cr,
                xx,
                34. - (i % 3) as f64 * 4.,
                16.,
                10. + (i % 3) as f64 * 4.,
                0x668c87,
            );
        }
    }
    rect(cr, x + window_w / 2., 10., 3., 34., p.wood);
    rect(cr, x - 6., 43., window_w + 12., 6., p.wood);
    rect(cr, x - 6., 43., window_w + 12., 2., p.accent);
    if theme != 2 && theme != 4 {
        for side in [x - 5., x + window_w - 7.] {
            rect(cr, side, 8., 12., 31., 0xf0e5cf);
            rect(cr, side + 4., 8., 2., 29., 0xd5c9ae);
            rect(cr, side + 1., 30., 10., 3., p.seat);
        }
    }
    wall_piece(cr, 18., 12., theme, design % 4, p);
    wall_piece(cr, w - 76., 12., theme, (design + 1) % 4, p);
}

fn wall_piece(cr: &Context, x: f64, y: f64, theme: usize, variant: usize, p: &Palette) {
    rect(cr, x + 2., y + 3., 56., 34., 0x26343c);
    match theme {
        0 => {
            rect(cr, x, y, 56., 34., p.wood);
            rect(cr, x + 3., y + 3., 50., 28., 0x537079);
            for i in 0..5 {
                let yy = y + 6. + ((i + variant) % 3) as f64 * 7.;
                rect(
                    cr,
                    x + 5. + i as f64 * 9.,
                    yy,
                    7.,
                    7.,
                    [0xedcd87, 0xd98c79, 0xa6c8b1][i % 3],
                );
            }
        }
        1 => {
            for i in 0..3 {
                let xx = x + i as f64 * 19.;
                rect(cr, xx + 6., y, 2., 14., p.accent);
                rect(cr, xx + 2., y + 10., 12., 11., 0x68926c);
                rect(cr, xx, y + 14., 16., 6., 0x9bbc85);
                rect(cr, xx + 4., y + 21., 9., 7., p.wood);
            }
        }
        2 => {
            rect(cr, x, y, 56., 34., 0x1d2f42);
            for i in 0..4 {
                rect(cr, x + 4., y + 5. + i as f64 * 6., 46., 1., 0x3e6977);
                rect(
                    cr,
                    x + 9. + ((i + variant) % 4) as f64 * 9.,
                    y + 4. + i as f64 * 6.,
                    5.,
                    3.,
                    p.accent,
                );
            }
        }
        3 => {
            for row in 0..2 {
                for i in 0..8 {
                    let xx = x + 3. + i as f64 * 6.;
                    let yy = y + row as f64 * 16.;
                    rect(
                        cr,
                        xx,
                        yy + 2.,
                        4.,
                        11.,
                        [p.accent, p.seat, 0xba8093, 0x789aa9][(i + variant) % 4],
                    );
                }
                rect(cr, x, y + 14. + row as f64 * 16., 56., 3., p.wood);
            }
        }
        4 => {
            rect(cr, x, y, 56., 34., p.wood);
            for i in 0..6 {
                rect(cr, x + 5. + i as f64 * 9., y + 4., 2., 2., 0x655d54);
            }
            for i in 0..3 {
                let xx = x + 8. + i as f64 * 17.;
                rect(cr, xx, y + 11., 3., 18., 0x485361);
                rect(cr, xx - 3., y + 8., 9., 5., 0xc1c9c5);
            }
        }
        _ => {
            rect(cr, x, y, 56., 34., 0xe6d7bb);
            rect(cr, x + 3., y + 3., 50., 28., p.rug);
            for i in 0..3 {
                rect(
                    cr,
                    x + 7.,
                    y + 7. + i as f64 * 8.,
                    18. + ((i + variant) % 3) as f64 * 7.,
                    2.,
                    p.accent,
                );
            }
        }
    }
}

pub(super) fn fixture(cr: &Context, area: Rect, theme: usize, variant: usize, frame: u32) {
    let p = &PALETTES[theme];
    let (x, y) = (area.x, area.y);
    rect(cr, x + 3., y + 5., area.w, area.h, 0x3b4650);
    rect(cr, x, y, area.w, area.h, p.wood);
    rect(cr, x, y, area.w, 3., p.accent);
    for xx in [x + 4., x + area.w - 8.] {
        rect(cr, xx, y + area.h - 3., 4., 6., 0x35404c);
    }
    match theme {
        0 => {
            // A project table with an upright sketch rack and material samples.
            rect(cr, x + 5., y - 24., 36., 30., 0x4e5c68);
            rect(cr, x + 8., y - 21., 30., 24., 0xf0debb);
            for i in 0..3 {
                rect(
                    cr,
                    x + 12.,
                    y - 17. + i as f64 * 6.,
                    12. + (i % 2) as f64 * 8.,
                    2.,
                    p.seat,
                );
            }
            for i in 0..3 {
                rect(
                    cr,
                    x + 48. + i as f64 * 8.,
                    y + 4. - i as f64 * 2.,
                    6.,
                    8.,
                    [p.seat, p.rug, p.accent][i],
                );
            }
        }
        1 => {
            // A planted glass propagation cabinet.
            rect(cr, x + 4., y - 30., 70., 36., 0x354e52);
            rect(cr, x + 7., y - 27., 64., 28., 0x78adb0);
            rect(cr, x + 7., y - 3., 64., 4., 0x586953);
            for i in 0..5 {
                let xx = x + 14. + i as f64 * 12.;
                let hh = 10. + ((i + variant) % 3) as f64 * 5.;
                rect(cr, xx, y - hh, 3., hh, 0x466c51);
                rect(cr, xx - 4., y - hh + 3., 11., 4., 0xc1d890);
            }
            rect(cr, x + 10., y - 25., 2., 19., 0xc4e1db);
        }
        2 => {
            // Twin telemetry cabinets, with a shared illuminated readout.
            for i in 0..2 {
                let xx = x + 6. + i as f64 * 38.;
                rect(cr, xx, y - 35., 30., 48., 0x243346);
                rect(cr, xx + 3., y - 32., 24., 17., 0x426f84);
                for j in 0..3 {
                    rect(cr, xx + 5., y - 10. + j as f64 * 6., 18., 3., 0x101f30);
                    rect(
                        cr,
                        xx + 23.,
                        y - 10. + j as f64 * 6.,
                        2.,
                        2.,
                        if (frame as usize + j).is_multiple_of(3) {
                            p.accent
                        } else {
                            0x5b878e
                        },
                    );
                }
            }
        }
        3 => {
            // A low archive with slanted folios and a reading lamp.
            for i in 0..8 {
                let xx = x + 5. + i as f64 * 6.;
                rect(
                    cr,
                    xx,
                    y - 14. - (i % 3) as f64 * 2.,
                    5.,
                    18. + (i % 3) as f64 * 2.,
                    [p.seat, p.accent, 0xb87782][(i + variant) % 3],
                );
                rect(cr, xx + 1., y - 7., 3., 1., 0xf2d5b0);
            }
            rect(cr, x + 64., y - 28., 3., 35., 0x3a404b);
            rect(cr, x + 57., y - 28., 18., 9., p.accent);
            rect(cr, x + 60., y + 7., 13., 3., 0x3a404b);
        }
        4 => {
            // A small fabrication bench: vice, drawer chest, and suspended tools.
            rect(cr, x + 4., y - 29., 72., 23., 0x647477);
            for i in 0..4 {
                rect(cr, x + 12. + i as f64 * 16., y - 25., 3., 16., p.accent);
                rect(cr, x + 9. + i as f64 * 16., y - 25., 9., 4., 0xc5ceca);
            }
            rect(cr, x + 5., y + 2., 23., 12., 0x354753);
            rect(cr, x + 4., y - 2., 27., 5., 0x93aeb0);
            rect(cr, x + 45., y - 1., 26., 15., 0xb66849);
            rect(cr, x + 49., y + 4., 18., 2., p.accent);
        }
        _ => {
            // Tea counter: glazed brewer, stacked cups, and pastry cloche.
            rect(cr, x + 5., y - 24., 29., 32., 0xeee0c3);
            rect(cr, x + 9., y - 18., 21., 15., p.rug);
            rect(cr, x + 13., y - 1., 11., 10., 0x82614d);
            rect(cr, x + 10., y + 9., 19., 3., 0x4a4d51);
            for i in 0..3 {
                rect(cr, x + 41. + i as f64 * 9., y + 2., 7., 8., p.accent);
            }
            rect(cr, x + 58., y - 17., 14., 13., 0xa7c5bd);
            rect(cr, x + 54., y - 4., 22., 4., 0xe9d2ac);
        }
    }
}
