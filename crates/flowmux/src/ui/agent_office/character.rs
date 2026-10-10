// SPDX-License-Identifier: GPL-3.0-or-later
//! Chibi animal teammates: twelve species, ten outfits, and every pose the office uses.

use super::sprite::{self, dark, light, mix, Canvas};
use gtk::cairo::ImageSurface;

pub(super) const SPECIES: [&str; 12] = [
    "Cat", "Bunny", "Bear", "Fox", "Panda", "Frog", "Penguin", "Owl", "Puppy", "Koala", "Piglet",
    "Axolotl",
];
const FUR: [u32; 12] = [
    0xeaa65a, 0xf1e6dc, 0xa8714a, 0xe9813f, 0xf2efe6, 0x8cc46e, 0x4a5876, 0xb69067, 0xd9b27a,
    0xa9b3c1, 0xf2a9a6, 0xe4b1e2,
];
const SHIRTS: [u32; 10] = [
    0x5aa68a, 0x3f5a8c, 0xd2686c, 0x6c8fd6, 0xe0a63e, 0x8a63b8, 0x45a9b8, 0xcf6f9e, 0xeef0ea,
    0x7da450,
];
const PANTS: [u32; 10] = [
    0x3d4466, 0x3a3c48, 0x7c6a52, 0x40527d, 0x3d4466, 0x524a63, 0x6b5a48, 0x3a3c48, 0x46557a,
    0x5b4e44,
];
pub(super) const INK: u32 = 0x2a2233;
const WHITE: u32 = 0xfffdf6;
const BLUSH: u32 = 0xf28a8f;
pub(super) const WIDTH: i32 = 24;
pub(super) const HEIGHT: i32 = 36;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) enum Dir {
    Front,
    Back,
    Side,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) enum Pose {
    Stand(u8),
    Walk(u8),
    Type(u8),
    Read(u8),
    Sit(u8),
    Coffee(u8),
    Browse(u8),
    Gaze,
    Tend(u8),
    Present(u8),
    Wave(u8),
}

struct Look {
    species: usize,
    outfit: usize,
    fur: u32,
    accent: u32,
    shirt: u32,
    pants: u32,
    hand: u32,
}

fn look(style: usize) -> Look {
    let species = style % 12;
    let outfit = style / 12 % 10;
    let fur = FUR[species];
    Look {
        species,
        outfit,
        fur,
        accent: match species {
            4 | 1 => 0xfffaf2,
            6 => WHITE,
            _ => light(fur, 0.55),
        },
        shirt: SHIRTS[outfit],
        pants: PANTS[outfit],
        hand: match species {
            4 => 0x34303c,
            6 => 0x3b4560,
            _ => fur,
        },
    }
}

/// Baked sprite for a teammate. `flip` mirrors the side view to face left.
pub(super) fn sprite(style: usize, dir: Dir, flip: bool, pose: Pose, blink: bool) -> ImageSurface {
    sprite::cached(("character", style % 120, dir, flip, pose, blink), || {
        let canvas = compose(style, dir, pose, blink);
        if flip {
            canvas.flipped()
        } else {
            canvas
        }
    })
}

fn seated(pose: Pose) -> bool {
    matches!(pose, Pose::Type(_) | Pose::Read(_) | Pose::Sit(_))
}

fn compose(style: usize, dir: Dir, pose: Pose, blink: bool) -> Canvas {
    let l = look(style);
    let mut c = Canvas::new(WIDTH, HEIGHT);
    let dy = match pose {
        _ if seated(pose) => 3,
        Pose::Walk(f) if f % 2 == 1 => -1,
        _ => 0,
    };
    let breath = match pose {
        Pose::Stand(f) | Pose::Sit(f) | Pose::Coffee(f) => i32::from(f % 2 == 1),
        _ => 0,
    };
    if dir != Dir::Back {
        tail(&mut c, &l, dir, dy);
    }
    legs(&mut c, &l, dir, pose);
    torso(&mut c, &l, dir, dy);
    if dir == Dir::Back {
        tail(&mut c, &l, dir, dy);
    }
    arms(&mut c, &l, dir, pose, dy);
    head(&mut c, &l, dir, dy + breath, blink);
    accessory(&mut c, &l, dir, dy + breath);
    held(&mut c, dir, pose, dy);
    c.outline(0.74);
    c
}

fn legs(c: &mut Canvas, l: &Look, dir: Dir, pose: Pose) {
    let shoe = 0x2f2a3a;
    let sole = 0x6b6577;
    if seated(pose) {
        if dir == Dir::Back {
            return;
        }
        for x in [7, 13] {
            c.rect(x, 31, 4, 3, l.pants);
            c.rect(x, 34, 4, 2, shoe);
        }
        return;
    }
    let frame = match pose {
        Pose::Walk(f) => f % 4,
        _ => 1,
    };
    if dir == Dir::Side {
        let far = dark(l.pants, 0.25);
        let (back, front) = match frame {
            0 => (8, 13),
            2 => (13, 8),
            _ => (10, 11),
        };
        let lift = i32::from(frame % 2 == 1);
        c.rect(back, 29, 3, 5, far);
        c.rect(back, 34, 4, 2, dark(shoe, 0.2));
        c.rect(front, 29, 3, 5 - lift, l.pants);
        c.rect(front, 34 - lift, 4, 2, shoe);
        c.hline(front + 1, 35 - lift, 3, sole);
        return;
    }
    for (index, x) in [7, 13].into_iter().enumerate() {
        let raised = match frame {
            0 => index == 1,
            2 => index == 0,
            _ => false,
        };
        let lift = if raised { 2 } else { 0 };
        c.rect(x, 29, 4, 5 - lift, l.pants);
        c.rect(x - 1 + index as i32 * 2 - 1 + 1, 34 - lift, 4, 2, shoe);
        if dir == Dir::Front {
            c.hline(x + index as i32 - 1 + 1, 34 - lift, 2, sole);
        }
    }
}

fn torso(c: &mut Canvas, l: &Look, dir: Dir, dy: i32) {
    let shade = dark(l.shirt, 0.22);
    let y = 19 + dy;
    if dir == Dir::Side {
        c.round(8, y, 9, 10, 2, l.shirt);
        c.vline(8, y + 2, 7, shade);
        detail(c, l, dir, 8, y);
        return;
    }
    c.round(6, y, 12, 10, 2, l.shirt);
    c.vline(17, y + 2, 7, shade);
    c.hline(7, y + 9, 10, shade);
    detail(c, l, dir, 6, y);
}

/// Outfit features drawn over the torso; each reads in silhouette from the front and back.
fn detail(c: &mut Canvas, l: &Look, dir: Dir, x: i32, y: i32) {
    let w = if dir == Dir::Side { 9 } else { 12 };
    let cx = x + w / 2;
    let front = dir == Dir::Front;
    match l.outfit {
        0 => {
            // Hoodie: hood folds at the neck, front pouch and drawstrings.
            c.hline(x + 1, y + 1, w - 2, dark(l.shirt, 0.3));
            if front {
                c.round(cx - 3, y + 5, 6, 3, 1, light(l.shirt, 0.15));
                c.vline(cx - 2, y + 2, 2, WHITE);
                c.vline(cx + 1, y + 2, 2, WHITE);
            } else if dir == Dir::Back {
                c.round(cx - 4, y, 8, 4, 1, dark(l.shirt, 0.18));
            }
        }
        1 => {
            // Blazer with shirt and tie.
            if front {
                c.rect(cx - 2, y, 4, 5, WHITE);
                c.vline(cx - 1, y + 1, 5, 0xc9474f);
                c.vline(cx, y + 1, 5, 0xa83a44);
                c.set(cx - 3, y + 6, 0xd9c26a);
            } else {
                c.vline(cx, y + 5, 5, dark(l.shirt, 0.3));
            }
        }
        2 => {
            // Knit stripes.
            for row in [y + 3, y + 6] {
                c.hline(x + 1, row, w - 2, light(l.shirt, 0.45));
            }
        }
        3 => {
            // Denim overalls with straps and buttons.
            let denim = 0x4d6ea6;
            c.rect(x + 1, y + 5, w - 2, 5, denim);
            if dir != Dir::Side {
                c.rect(cx - 3, y + 2, 6, 3, denim);
                c.vline(x + 2, y, 3, denim);
                c.vline(x + w - 3, y, 3, denim);
                if front {
                    c.set(cx - 3, y + 2, 0xf2c84b);
                    c.set(cx + 2, y + 2, 0xf2c84b);
                }
            }
        }
        4 => {
            // Apron.
            if front {
                c.rect(cx - 4, y + 3, 8, 7, 0xf3e7cf);
                c.hline(cx - 4, y + 3, 8, 0xe2cfaa);
                c.rect(cx - 1, y + 6, 2, 2, l.shirt);
            } else {
                c.hline(x, y + 4, w, 0xf3e7cf);
                c.rect(cx - 1, y + 4, 2, 3, 0xf3e7cf);
            }
        }
        5 => {
            // Varsity jacket: cream trims and a chest letter.
            c.hline(x + 1, y + 9, w - 2, 0xf3e7cf);
            if front {
                c.vline(cx, y + 1, 8, 0xf3e7cf);
                c.rect(x + 2, y + 3, 2, 3, 0xf3e7cf);
            } else if dir == Dir::Back {
                c.rect(cx - 2, y + 3, 4, 4, 0xf3e7cf);
            }
        }
        6 => {
            // Graphic tee.
            if front {
                c.oval(cx - 2, y + 3, 5, 4, light(l.shirt, 0.6));
                c.set(cx, y + 4, l.shirt);
            }
        }
        7 => {
            // Cardigan and a long scarf.
            let scarf = 0xe5b54a;
            c.rect(x, y, w, 2, scarf);
            if front {
                c.rect(cx + 2, y + 2, 2, 5, scarf);
                c.hline(cx + 2, y + 7, 2, dark(scarf, 0.3));
                c.vline(cx - 1, y + 3, 6, dark(l.shirt, 0.25));
            } else if dir == Dir::Side {
                c.rect(x - 1, y + 2, 2, 4, scarf);
            }
        }
        8 => {
            // Lab coat over a colored shirt.
            if front {
                c.rect(cx - 1, y, 2, 10, 0x6f9fd0);
                c.vline(cx - 2, y + 1, 9, 0xd5dbe3);
                c.vline(cx + 1, y + 1, 9, 0xd5dbe3);
                c.rect(x + 2, y + 4, 2, 2, 0x6f9fd0);
            }
        }
        _ => {
            // Sweater vest over a shirt collar.
            let vest = dark(l.shirt, 0.15);
            c.rect(x + 1, y + 2, w - 2, 8, vest);
            if front {
                c.hline(x + 2, y, 3, WHITE);
                c.hline(x + w - 5, y, 3, WHITE);
                c.set(cx - 1, y + 2, l.shirt);
                c.set(cx, y + 2, l.shirt);
                c.set(cx - 1, y + 3, l.shirt);
            }
        }
    }
}

fn sleeve(l: &Look) -> u32 {
    match l.outfit {
        5 => 0xf3e7cf,
        8 => 0xeef0ea,
        _ => l.shirt,
    }
}

fn arms(c: &mut Canvas, l: &Look, dir: Dir, pose: Pose, dy: i32) {
    let sleeve = sleeve(l);
    let inner = dark(sleeve, 0.2);
    let y = 20 + dy;
    if dir == Dir::Side {
        let (x, hand_y, hand_x, reach) = match pose {
            Pose::Walk(0) => (12, y + 6, 13, false),
            Pose::Walk(2) => (8, y + 6, 8, false),
            Pose::Tend(_) => (12, y + 1, 15, true),
            _ => (10, y + 6, 10, false),
        };
        if reach {
            c.rect(x, y + 1, 4, 3, sleeve);
            c.rect(hand_x, y + 1, 2, 3, l.hand);
        } else {
            c.rect(x, y, 3, 6, sleeve);
            c.vline(x, y + 1, 5, inner);
            c.rect(hand_x, hand_y, 3, 2, l.hand);
        }
        return;
    }
    let swing = match pose {
        Pose::Walk(0) => (1, -1),
        Pose::Walk(2) => (-1, 1),
        _ => (0, 0),
    };
    let (left_x, right_x) = (3, 18);
    let mut arm = |x: i32, lift: i32, inner_x: i32| {
        c.rect(x, y + lift, 3, 7, sleeve);
        c.vline(inner_x, y + lift + 1, 6, inner);
        c.rect(x, y + lift + 7, 3, 2, l.hand);
    };
    match (dir, pose) {
        (Dir::Front, Pose::Wave(f)) => {
            arm(left_x, 0, 5);
            let shift = i32::from(f % 2 == 1);
            c.rect(right_x + shift, 12 + dy, 3, 8, sleeve);
            c.rect(right_x + shift, 10 + dy, 3, 2, l.hand);
        }
        (Dir::Front, Pose::Type(f)) => {
            let beat = i32::from(f % 2 == 1);
            for (x, hand, lift) in [(4, 7, beat), (17, 14, 1 - beat)] {
                c.rect(x, y + 1, 3, 5, sleeve);
                c.rect(hand, y + 5 - lift, 3, 2, l.hand);
            }
        }
        (Dir::Back, Pose::Type(f)) => {
            let beat = i32::from(f % 2 == 1);
            c.rect(left_x, y + 1 - beat, 3, 5, sleeve);
            c.rect(right_x, y + beat, 3, 5, sleeve);
        }
        (Dir::Front, Pose::Read(_)) => {
            for (x, hand) in [(4, 7), (17, 14)] {
                c.rect(x, y + 1, 3, 5, sleeve);
                c.rect(hand, y + 6, 3, 2, l.hand);
            }
        }
        (Dir::Back, Pose::Read(_)) | (Dir::Back, Pose::Browse(_)) | (_, Pose::Present(_)) => {
            arm(left_x, 0, 5);
            let reach = match pose {
                Pose::Browse(f) | Pose::Present(f) => i32::from(f % 2 == 1),
                _ => 0,
            };
            c.rect(right_x, 14 + dy - reach, 3, 7, sleeve);
            c.rect(right_x, 12 + dy - reach, 3, 2, l.hand);
        }
        (Dir::Front, Pose::Coffee(f)) => {
            arm(left_x, 0, 5);
            if f % 4 >= 2 {
                c.rect(right_x - 1, 17 + dy, 3, 5, sleeve);
                c.rect(right_x - 3, 16 + dy, 3, 2, l.hand);
            } else {
                c.rect(right_x, y, 3, 5, sleeve);
                c.rect(right_x - 1, y + 5, 3, 2, l.hand);
            }
        }
        (Dir::Back, Pose::Gaze) => {
            c.rect(left_x, y, 3, 6, sleeve);
            c.rect(right_x, y, 3, 6, sleeve);
            c.rect(8, y + 6, 8, 2, l.hand);
        }
        _ => {
            arm(left_x, swing.0, 5);
            arm(right_x, swing.1, 18);
        }
    }
}

fn head(c: &mut Canvas, l: &Look, dir: Dir, dy: i32, blink: bool) {
    let y = 6 + dy;
    let fur = l.fur;
    let shade = dark(fur, 0.16);
    let (x, w) = if dir == Dir::Side { (4, 16) } else { (3, 18) };
    ears(c, l, dir, x, y, w);
    c.round(x, y, w, 15, 4, fur);
    // Soft volume: a warm top rim and a cooler chin.
    c.hline(x + 4, y, w - 8, light(fur, 0.22));
    c.hline(x + 3, y + 13, w - 6, shade);
    c.hline(x + 5, y + 14, w - 10, shade);
    // Light from the upper left leaves the right cheek in soft shade.
    if dir != Dir::Side {
        c.vline(x + w - 2, y + 4, 8, shade);
        c.vline(x + w - 3, y + 11, 2, shade);
    }
    match dir {
        Dir::Front => face(c, l, x, y, blink),
        Dir::Side => profile(c, l, x, y, blink),
        Dir::Back => back_of_head(c, l, x, y),
    }
}

fn ears(c: &mut Canvas, l: &Look, dir: Dir, x: i32, y: i32, w: i32) {
    let fur = l.fur;
    let inner = mix(fur, BLUSH, 0.45);
    let side = dir == Dir::Side;
    // In profile only the far-side ear, set back on the crown, is drawn.
    let anchors: &[i32] = if side { &[x + 3] } else { &[x + 1, x + w - 6] };
    for (index, &ax) in anchors.iter().enumerate() {
        let right = !side && index == 1;
        match l.species {
            0 | 3 => {
                let big = l.species == 3;
                let height = if big { 6 } else { 5 };
                for row in 0..height {
                    let width = (row + 2).min(5 + i32::from(big));
                    let ex = if right { ax + 5 - width } else { ax };
                    c.hline(ex, y - height + 1 + row, width, fur);
                    if row >= 2 && dir != Dir::Back {
                        let ix = ex + 1;
                        c.hline(
                            ix,
                            y - height + 1 + row,
                            (width - 2).max(1),
                            if big { 0xfff1e0 } else { inner },
                        );
                    }
                }
                if big {
                    let tip = if right { ax + 4 } else { ax };
                    c.set(tip, y - height + 1, INK);
                }
            }
            1 => {
                let ex = ax + 2;
                c.round(ex, y - 8, 4, 10, 1, fur);
                if dir != Dir::Back {
                    c.rect(ex + 1, y - 7, 2, 7, inner);
                }
            }
            2 | 9 | 4 => {
                let (size, inner_color) = match l.species {
                    9 => (8, light(fur, 0.45)),
                    4 => (6, 0x34303c),
                    _ => (6, dark(fur, 0.3)),
                };
                let ex = if right {
                    ax + 6 - size
                } else {
                    ax - 1 - (size - 6) / 2
                };
                let color = if l.species == 4 { 0x34303c } else { fur };
                c.oval(ex, y - 3, size, size, color);
                if l.species != 4 {
                    c.oval(ex + 2, y - 1, size - 4, size - 4, inner_color);
                }
            }
            5 => {
                // Frog eyes sit on top of the head instead of ears.
                let ex = if right { ax - 1 } else { ax };
                c.oval(ex, y - 4, 7, 7, fur);
                if dir == Dir::Front {
                    c.oval(ex + 1, y - 3, 5, 5, WHITE);
                }
            }
            7 => {
                let ex = if right { ax + 3 } else { ax };
                c.rect(ex, y - 2, 3, 3, dark(fur, 0.2));
                c.set(if right { ex + 2 } else { ex }, y - 3, dark(fur, 0.2));
            }
            8 => {
                // Floppy ears hang beside the cheeks.
                let ex = if side {
                    ax + 1
                } else if right {
                    x + w - 2
                } else {
                    x - 2
                };
                c.round(ex, y + 2, 4, 9, 1, 0x7d5a43);
            }
            10 => {
                let ex = if right { ax + 2 } else { ax + 1 };
                c.rect(ex, y - 2, 4, 3, dark(fur, 0.12));
                c.set(if right { ex } else { ex + 3 }, y + 1, dark(fur, 0.12));
            }
            11 => {
                // Axolotl gills: three fronds per side.
                let ex = if side {
                    x - 3
                } else if right {
                    x + w
                } else {
                    x - 3
                };
                for (i, row) in [3, 7, 11].into_iter().enumerate() {
                    let reach = 3 - i32::from(i == 1);
                    c.rect(
                        if right { ex } else { ex + 3 - reach },
                        y + row,
                        reach,
                        2,
                        0xd9548f,
                    );
                }
            }
            _ => {}
        }
    }
}

fn eyes(c: &mut Canvas, points: &[(i32, i32)], blink: bool, pupil: u32) {
    for &(x, y) in points {
        if blink {
            c.hline(x, y + 1, 2, pupil);
        } else {
            c.rect(x, y, 2, 2, pupil);
            c.set(x + 1, y, WHITE);
        }
    }
}

fn face(c: &mut Canvas, l: &Look, x: i32, y: i32, blink: bool) {
    let fur = l.fur;
    let (le, re) = ((x + 4, y + 7), (x + 12, y + 7));
    let blush = mix(fur, BLUSH, 0.6);
    let mouth = (x + 8, y + 10);
    match l.species {
        2 | 8 => {
            c.oval(x + 5, y + 8, 8, 6, l.accent);
            c.rect(mouth.0, mouth.1 - 1, 2, 2, INK);
            c.set(mouth.0 - 1, mouth.1 + 2, INK);
            c.set(mouth.0 + 2, mouth.1 + 2, INK);
            if l.species == 8 {
                c.oval(x + 11, y + 4, 5, 5, dark(fur, 0.28));
                c.set(mouth.0 + 1, mouth.1 + 3, 0xe8737c);
            }
            eyes(c, &[le, re], blink, INK);
        }
        3 => {
            c.oval(x, y + 8, 7, 7, l.accent);
            c.oval(x + w_of(l) - 7, y + 8, 7, 7, l.accent);
            c.oval(x + 6, y + 10, 6, 5, l.accent);
            c.rect(mouth.0, mouth.1, 2, 1, INK);
            eyes(c, &[le, re], blink, INK);
        }
        4 => {
            c.oval(le.0 - 2, le.1 - 2, 6, 6, 0x34303c);
            c.oval(re.0 - 2, re.1 - 2, 6, 6, 0x34303c);
            eyes(c, &[le, re], blink, 0x111018);
            c.rect(mouth.0, mouth.1, 2, 1, INK);
        }
        5 => {
            // Eyes are on the head-top bumps; the face holds a wide grin.
            let (l1, r1) = ((x + 2, y - 2), (x + 13, y - 2));
            eyes(c, &[l1, r1], blink, INK);
            c.hline(x + 5, y + 10, 8, INK);
            c.set(x + 4, y + 9, INK);
            c.set(x + 13, y + 9, INK);
        }
        6 => {
            c.oval(x + 3, y + 3, 6, 9, WHITE);
            c.oval(x + 9, y + 3, 6, 9, WHITE);
            c.rect(x + 4, y + 8, 10, 6, WHITE);
            c.rect(mouth.0, mouth.1 - 1, 2, 2, 0xf0a33a);
            c.hline(mouth.0 - 1, mouth.1 - 1, 4, 0xf0a33a);
            eyes(c, &[le, re], blink, INK);
        }
        7 => {
            c.oval(le.0 - 2, le.1 - 2, 6, 6, 0xf6e7c2);
            c.oval(re.0 - 2, re.1 - 2, 6, 6, 0xf6e7c2);
            eyes(c, &[le, re], blink, INK);
            c.rect(mouth.0, mouth.1 - 1, 2, 2, 0xe1a23c);
        }
        9 => {
            eyes(c, &[le, re], blink, INK);
            c.round(mouth.0 - 1, mouth.1 - 3, 4, 5, 1, 0x3b3646);
            c.set(mouth.0, mouth.1 - 3, 0x6b6577);
        }
        10 => {
            eyes(c, &[le, re], blink, INK);
            c.round(mouth.0 - 2, mouth.1 - 2, 6, 4, 1, dark(fur, 0.12));
            c.set(mouth.0 - 1, mouth.1 - 1, dark(fur, 0.45));
            c.set(mouth.0 + 2, mouth.1 - 1, dark(fur, 0.45));
        }
        11 => {
            eyes(c, &[(le.0 - 1, le.1), (re.0 + 1, re.1)], blink, INK);
            c.hline(x + 6, y + 10, 6, INK);
            c.set(x + 5, y + 9, INK);
            c.set(x + 12, y + 9, INK);
        }
        _ => {
            // Cat: forehead stripes and a small nose over a "w" mouth.
            for sx in [x + 7, x + 9, x + 11] {
                c.vline(sx, y + 1, 2, dark(fur, 0.25));
            }
            eyes(c, &[le, re], blink, INK);
            c.rect(mouth.0, mouth.1 - 1, 2, 1, 0xd96f7d);
            c.set(mouth.0 - 1, mouth.1, INK);
            c.set(mouth.0 + 2, mouth.1, INK);
        }
    }
    if !matches!(l.species, 4 | 6) {
        c.hline(x + 2, y + 10, 2, blush);
        c.hline(x + 14, y + 10, 2, blush);
    }
}

fn w_of(_: &Look) -> i32 {
    18
}

fn profile(c: &mut Canvas, l: &Look, x: i32, y: i32, blink: bool) {
    let fur = l.fur;
    let eye = (x + 10, y + 7);
    match l.species {
        2 | 3 | 8 | 9 | 10 => {
            let color = match l.species {
                10 => dark(fur, 0.12),
                9 => fur,
                _ => l.accent,
            };
            c.round(x + 12, y + 8, 6, 5, 1, color);
            let nose = if l.species == 9 { 0x3b3646 } else { INK };
            c.rect(x + 17, y + 8, 1, 2, nose);
        }
        6 => {
            c.oval(x + 7, y + 3, 9, 10, WHITE);
            c.rect(x + 16, y + 9, 2, 2, 0xf0a33a);
        }
        7 => {
            c.oval(eye.0 - 2, eye.1 - 2, 6, 6, 0xf6e7c2);
            c.rect(x + 16, y + 9, 1, 2, 0xe1a23c);
        }
        5 => {
            c.hline(x + 9, y + 10, 6, INK);
        }
        _ => {
            c.set(x + 15, y + 9, 0xd96f7d);
        }
    }
    let eye = if l.species == 5 { (x + 9, y - 2) } else { eye };
    if l.species == 5 {
        c.oval(eye.0 - 2, eye.1 - 2, 6, 6, WHITE);
    }
    if blink {
        c.hline(eye.0, eye.1 + 1, 2, INK);
    } else {
        c.rect(eye.0, eye.1, 1, 2, INK);
        c.set(eye.0 + 1, eye.1, INK);
        c.set(eye.0 + 1, eye.1 + 1, WHITE);
    }
    if !matches!(l.species, 4 | 6) {
        c.hline(x + 10, y + 10, 2, mix(fur, BLUSH, 0.6));
    }
    if l.species == 4 {
        c.oval(eye.0 - 2, eye.1 - 2, 5, 6, 0x34303c);
        c.set(eye.0 + 1, eye.1, WHITE);
    }
}

fn back_of_head(c: &mut Canvas, l: &Look, x: i32, y: i32) {
    let fur = l.fur;
    match l.species {
        6 => c.round(x + 2, y + 1, 14, 12, 3, dark(fur, 0.12)),
        0 => {
            for sx in [x + 6, x + 9, x + 12] {
                c.vline(sx, y + 2, 3, dark(fur, 0.25));
            }
        }
        _ => {
            c.vline(x + 9, y + 3, 2, dark(fur, 0.14));
        }
    }
}

fn tail(c: &mut Canvas, l: &Look, dir: Dir, dy: i32) {
    let fur = l.fur;
    let y = 24 + dy;
    match (l.species, dir) {
        (3, Dir::Back) => {
            c.oval(13, y, 8, 7, fur);
            c.oval(17, y + 4, 4, 4, 0xfff1e0);
        }
        (3, Dir::Side) => {
            c.oval(1, y - 1, 8, 6, fur);
            c.oval(1, y + 1, 3, 3, 0xfff1e0);
        }
        (3, Dir::Front) => {
            c.oval(17, y - 1, 7, 6, fur);
            c.oval(20, y - 1, 3, 3, 0xfff1e0);
        }
        (0, Dir::Back) => {
            c.vline(16, y - 2, 6, fur);
            c.vline(17, y - 4, 3, fur);
            c.set(17, y - 5, dark(fur, 0.25));
        }
        (0, Dir::Side) => {
            c.line((7, y + 3), (3, y - 3), fur);
            c.line((6, y + 3), (2, y - 3), fur);
        }
        (1 | 4 | 2, Dir::Back) => c.oval(10, y + 2, 4, 4, if l.species == 2 { fur } else { WHITE }),
        (10, Dir::Back) => {
            c.set(12, y + 2, dark(fur, 0.15));
            c.set(13, y + 1, dark(fur, 0.15));
            c.set(12, y, dark(fur, 0.15));
        }
        (8, Dir::Back) => c.rect(11, y, 2, 4, fur),
        (7 | 6, Dir::Back) => c.rect(10, y + 3, 4, 2, dark(fur, 0.2)),
        (11, Dir::Back | Dir::Side) => {
            c.oval(if dir == Dir::Side { 2 } else { 10 }, y + 2, 5, 4, fur)
        }
        _ => {}
    }
}

fn accessory(c: &mut Canvas, l: &Look, dir: Dir, dy: i32) {
    let y = 6 + dy;
    match l.outfit {
        6 => {
            // Headphones: a band over the crown and ear cups at the sides.
            let band = 0x3c3a4a;
            match dir {
                Dir::Side => {
                    c.vline(10, y - 1, 7, band);
                    c.round(8, y + 5, 5, 5, 1, 0xe25a63);
                }
                _ => {
                    c.hline(5, y - 1, 14, band);
                    c.round(1, y + 5, 4, 6, 1, 0xe25a63);
                    c.round(19, y + 5, 4, 6, 1, 0xe25a63);
                }
            }
        }
        9 if dir != Dir::Back => {
            // Round glasses.
            let frame = 0x3c3a4a;
            if dir == Dir::Front {
                for x in [5, 13] {
                    c.hline(x, y + 6, 4, frame);
                    c.hline(x, y + 9, 4, frame);
                    c.vline(x - 1, y + 7, 2, frame);
                    c.vline(x + 4, y + 7, 2, frame);
                }
                c.hline(10, y + 7, 2, frame);
            } else {
                c.hline(13, y + 6, 4, frame);
                c.hline(13, y + 9, 4, frame);
                c.vline(17, y + 7, 2, frame);
            }
        }
        _ => {}
    }
}

fn held(c: &mut Canvas, dir: Dir, pose: Pose, dy: i32) {
    match (dir, pose) {
        (Dir::Front, Pose::Read(f)) => {
            c.rect(7, 23 + dy, 10, 7, 0x456aa6);
            c.rect(8, 24 + dy, 8, 5, 0xf4ecd9);
            c.vline(12, 24 + dy, 5, 0xc9bda5);
            if f % 2 == 1 {
                c.rect(12, 24 + dy, 3, 4, 0xfffaf0);
            }
            for row in [25, 27] {
                c.hline(9, row + dy, 2, 0xa59a86);
            }
        }
        (Dir::Back, Pose::Read(_)) => {
            c.rect(17, 9 + dy, 6, 8, 0xf4ecd9);
            c.hline(18, 11 + dy, 4, 0xa59a86);
            c.hline(18, 13 + dy, 3, 0xa59a86);
        }
        (Dir::Front, Pose::Coffee(f)) => {
            let (x, y) = if f % 4 >= 2 {
                (14, 14 + dy)
            } else {
                (16, 24 + dy)
            };
            c.rect(x, y, 4, 4, 0xf3ecdf);
            c.hline(x, y, 4, 0x8a5a3c);
            c.vline(x + 4, y + 1, 2, 0xf3ecdf);
            if f % 4 == 1 {
                c.set(x + 1, y - 2, 0xe8e4f0);
                c.set(x + 2, y - 3, 0xe8e4f0);
            }
        }
        (Dir::Side, Pose::Tend(f)) => {
            let tilt = i32::from(f % 2 == 1);
            c.round(15, 21 + dy, 5, 5, 1, 0x58a88f);
            c.line((19, 22 + dy), (22, 20 + dy + tilt * 2), 0x58a88f);
            if tilt == 1 {
                c.set(22, 25 + dy, 0x8fd2f0);
                c.set(21, 27 + dy, 0x8fd2f0);
            }
        }
        (_, Pose::Present(_)) => c.rect(19, 11 + dy, 2, 2, 0xd9484f),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn pixels(style: usize, dir: Dir, pose: Pose) -> Vec<u8> {
        snapshot(&sprite(style, dir, false, pose, false))
    }

    fn snapshot(sprite: &ImageSurface) -> Vec<u8> {
        let mut surface = ImageSurface::create(gtk::cairo::Format::ARgb32, WIDTH, HEIGHT).unwrap();
        {
            let cr = gtk::cairo::Context::new(&surface).unwrap();
            super::sprite::paint(&cr, sprite, 0.0, 0.0);
        }
        let data = surface.data().unwrap().to_vec();
        data
    }

    #[test]
    fn hundred_twenty_distinct_teammates_in_every_view() {
        for dir in [Dir::Front, Dir::Back, Dir::Side] {
            let looks: HashSet<_> = (0..120).map(|s| pixels(s, dir, Pose::Stand(0))).collect();
            assert_eq!(looks.len(), 120, "{dir:?}");
        }
    }

    #[test]
    fn poses_and_frames_change_the_figure() {
        let walk: HashSet<_> = (0..4)
            .map(|f| pixels(7, Dir::Side, Pose::Walk(f)))
            .collect();
        assert!(
            walk.len() >= 3,
            "a walk cycle needs distinct contact and passing frames"
        );
        let typing: HashSet<_> = (0..2)
            .map(|f| pixels(7, Dir::Back, Pose::Type(f)))
            .collect();
        assert_eq!(typing.len(), 2);
        assert_ne!(
            pixels(7, Dir::Front, Pose::Read(0)),
            pixels(7, Dir::Front, Pose::Sit(0))
        );
        assert_ne!(
            snapshot(&sprite(3, Dir::Front, false, Pose::Stand(0), true)),
            pixels(3, Dir::Front, Pose::Stand(0))
        );
    }
}
