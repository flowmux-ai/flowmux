// SPDX-License-Identifier: GPL-3.0-or-later
//! Original pixel artwork. Integer rectangles preserve crisp pixels at 2× scale.

use flowmux_core::AgentStatus;
use gtk::cairo::Context;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

pub(super) const WIDTH: i32 = 360;
pub(super) const SPECIES: [&str; 12] = [
    "Cat", "Bunny", "Bear", "Fox", "Panda", "Frog", "Penguin", "Owl", "Puppy", "Koala", "Piglet",
    "Axolotl",
];
const FUR: [u32; 12] = [
    0xe6b36c, 0xe9d9d0, 0xa87751, 0xe58e50, 0xe9e5d5, 0x91be76, 0x66778b, 0xb7a285, 0xc9ae84,
    0xaab6c2, 0xeeb3b0, 0xd5b0db,
];
const SHIRTS: [u32; 10] = [
    0x69a68b, 0xcb7375, 0x7e91c9, 0xd6ab50, 0xa48ac9, 0x65b3bc, 0xc582ab, 0x699cce, 0xa3b263,
    0xd79060,
];
const INK: u32 = 0x252c3b;

pub(super) fn height(count: usize) -> i32 {
    108 + count.div_ceil(2).max(2) as i32 * 84
}
fn desk(slot: usize) -> (f64, f64) {
    (
        44.0 + (slot % 3) as f64 * 68.0,
        108.0 + (slot / 3) as f64 * 84.0,
    )
}
fn lounge(slot: usize) -> (f64, f64) {
    (
        262.0 + (slot % 2) as f64 * 52.0,
        108.0 + (slot / 2) as f64 * 84.0,
    )
}
fn destination(slot: usize, status: AgentStatus) -> (f64, f64) {
    if matches!(status, AgentStatus::Idle | AgentStatus::Done) {
        lounge(slot)
    } else {
        desk(slot)
    }
}

pub(super) struct Actor {
    slot: usize,
    style: usize,
    pub(super) status: AgentStatus,
    pub(super) position: (f64, f64),
    path: VecDeque<(f64, f64)>,
}

impl Actor {
    pub(super) fn new(slot: usize, style: usize, status: AgentStatus) -> Self {
        Self {
            slot,
            style,
            status,
            position: destination(slot, status),
            path: VecDeque::new(),
        }
    }

    pub(super) fn set_status(&mut self, status: AgentStatus) {
        if self.status == status {
            return;
        }
        let old = destination(self.slot, self.status);
        let target = destination(self.slot, status);
        self.status = status;
        if old == target {
            return;
        }
        // Route through the clear aisle, including when a new event interrupts a walk.
        let aisle = 129.0 + ((self.position.1 - 108.0) / 84.0).floor().max(0.0) * 84.0;
        self.path = VecDeque::from([
            (self.position.0, aisle),
            (228.0, aisle),
            (228.0, target.1 + 21.0),
            (target.0, target.1 + 21.0),
            target,
        ]);
    }

    pub(super) fn advance(&mut self, dt: f64, animate: bool) -> bool {
        if !animate {
            let changed = !self.path.is_empty();
            self.position = destination(self.slot, self.status);
            self.path.clear();
            return changed;
        }
        let Some(&(x, y)) = self.path.front() else {
            return false;
        };
        let (dx, dy) = (x - self.position.0, y - self.position.1);
        let distance = dx.hypot(dy);
        let step = dt * 55.0;
        if distance <= step {
            self.position = (x, y);
            self.path.pop_front();
        } else {
            self.position.0 += dx / distance * step;
            self.position.1 += dy / distance * step;
        }
        true
    }
}

fn rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, color: u32) {
    cr.set_source_rgb(
        ((color >> 16) & 255) as f64 / 255.0,
        ((color >> 8) & 255) as f64 / 255.0,
        (color & 255) as f64 / 255.0,
    );
    cr.rectangle(x.round(), y.round(), w, h);
    let _ = cr.fill();
}
fn text(cr: &Context, x: f64, y: f64, value: &str, size: f64, color: u32) {
    cr.set_antialias(gtk::cairo::Antialias::Gray);
    cr.select_font_face(
        "monospace",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Bold,
    );
    cr.set_font_size(size);
    cr.set_source_rgb(
        ((color >> 16) & 255) as f64 / 255.0,
        ((color >> 8) & 255) as f64 / 255.0,
        (color & 255) as f64 / 255.0,
    );
    cr.move_to(x, y);
    let _ = cr.show_text(value);
    cr.set_antialias(gtk::cairo::Antialias::None);
}

pub(super) fn draw_room(cr: &Context, height: i32, actors: &[Rc<RefCell<Actor>>], frame: u32) {
    cr.set_antialias(gtk::cairo::Antialias::None);
    rect(cr, 0.0, 0.0, 360.0, height as f64, 0x171f2b);
    rect(cr, 8.0, 8.0, 344.0, 44.0, 0x39465a);
    rect(cr, 8.0, 50.0, 344.0, height as f64 - 60.0, 0x8d6546);
    for row in 0..(height - 60) / 8 {
        let y = 52.0 + row as f64 * 8.0;
        rect(cr, 8.0, y, 344.0, 1.0, 0x78563e);
        for col in 0..9 {
            let x = 8.0 + col as f64 * 42.0 + (row % 2) as f64 * 21.0;
            if x < 351.0 {
                rect(cr, x, y + 1.0, 1.0, 7.0, 0x78563e);
            }
        }
    }
    // A shared lounge and an open doorway connect the two sides of the room.
    rect(cr, 241.0, 52.0, 111.0, height as f64 - 62.0, 0x466775);
    for x in [244.0, 348.0] {
        rect(cr, x, 55.0, 1.0, height as f64 - 68.0, 0x789095);
    }
    rect(cr, 8.0, 48.0, 344.0, 4.0, 0x222d3c);
    rect(cr, 234.0, 8.0, 7.0, 83.0, 0x202a39);
    rect(cr, 234.0, 147.0, 7.0, (height - 157) as f64, 0x202a39);
    bookshelf(cr, 23.0, 17.0);
    bookshelf(cr, 167.0, 17.0);
    // Window with a little skyline.
    rect(cr, 101.0, 14.0, 46.0, 30.0, 0x202a39);
    rect(cr, 104.0, 17.0, 40.0, 24.0, 0x8ec3c5);
    rect(cr, 106.0, 29.0, 12.0, 12.0, 0x638d9d);
    rect(cr, 133.0, 23.0, 9.0, 18.0, 0x6c94a2);
    rect(cr, 123.0, 17.0, 2.0, 24.0, 0xd5d8c6);
    rect(cr, 104.0, 28.0, 40.0, 2.0, 0xd5d8c6);
    rect(cr, 275.0, 19.0, 49.0, 22.0, 0x202a39);
    text(cr, 283.0, 33.0, "BREAK", 8.0, 0xe5cf9a);
    let count = actors.len().max(3);
    for slot in 0..count {
        let (x, y) = desk(slot);
        let active = actors
            .get(slot)
            .is_some_and(|actor| actor.borrow().status == AgentStatus::Working);
        workstation(cr, x, y, active, frame);
    }
    for slot in 0..actors.len().max(2) {
        let (x, y) = lounge(slot);
        sofa(cr, x, y);
    }
    if actors.len() <= 3 {
        // The spare floor is a shared meeting corner until more desks are needed.
        rect(cr, 53.0, 156.0, 127.0, 63.0, 0x63786c);
        rect(cr, 56.0, 159.0, 121.0, 57.0, 0x879384);
        for x in [84.0, 137.0] {
            rect(cr, x, 150.0, 14.0, 17.0, 0x394650);
            rect(cr, x + 1.0, 150.0, 12.0, 12.0, 0xb18a63);
            rect(cr, x, 205.0, 14.0, 17.0, 0x394650);
            rect(cr, x + 1.0, 205.0, 12.0, 12.0, 0xb18a63);
        }
        rect(cr, 70.0, 170.0, 95.0, 36.0, 0x564939);
        rect(cr, 68.0, 165.0, 95.0, 34.0, 0xd1ab79);
        rect(cr, 70.0, 167.0, 91.0, 29.0, 0xbb9060);
        rect(cr, 99.0, 172.0, 13.0, 16.0, 0xe9ddbd);
        rect(cr, 102.0, 176.0, 7.0, 1.0, 0x929b93);
        rect(cr, 102.0, 179.0, 5.0, 1.0, 0x929b93);
        rect(cr, 125.0, 179.0, 6.0, 7.0, 0xf0dcb4);
    }
    let bottom = height as f64 - 33.0;
    plant(cr, 24.0, bottom + 12.0);
    plant(cr, 335.0, bottom + 12.0);
    // Coffee counter and shared noticeboard, rather than furniture per agent.
    rect(cr, 260.0, bottom - 5.0, 58.0, 24.0, 0x282c35);
    rect(cr, 258.0, bottom - 8.0, 62.0, 8.0, 0xd1ab79);
    rect(cr, 261.0, bottom, 56.0, 15.0, 0x9b7550);
    rect(cr, 266.0, bottom - 23.0, 16.0, 15.0, 0x202a39);
    rect(cr, 268.0, bottom - 20.0, 12.0, 6.0, 0x91a4a5);
    rect(cr, 271.0, bottom - 13.0, 6.0, 6.0, 0xe7ddc7);
    for x in [293.0, 305.0] {
        rect(cr, x, bottom - 13.0, 5.0, 5.0, 0xf1e0bb);
    }
    text(
        cr,
        54.0,
        bottom + 10.0,
        "MAKE SOMETHING GOOD",
        7.0,
        0xe4bd8b,
    );
    let mut sorted: Vec<_> = actors.iter().collect();
    sorted.sort_by(|a, b| a.borrow().position.1.total_cmp(&b.borrow().position.1));
    for actor in sorted {
        draw_actor(cr, &actor.borrow(), frame);
    }
}

fn bookshelf(cr: &Context, x: f64, y: f64) {
    rect(cr, x, y, 50.0, 29.0, 0x222936);
    for row in 0..2 {
        for i in 0..9 {
            let height = 7.0 + (i % 3) as f64;
            rect(
                cr,
                x + 3.0 + i as f64 * 5.0,
                y + 12.0 + row as f64 * 13.0 - height,
                3.0,
                height,
                [0xc78073, 0xddc08c, 0x819d86, 0x7a9cab][i % 4],
            );
        }
        rect(cr, x, y + 12.0 + row as f64 * 13.0, 50.0, 3.0, 0xb18a63);
    }
}
fn plant(cr: &Context, x: f64, y: f64) {
    rect(cr, x - 7.0, y - 8.0, 14.0, 10.0, 0x252d35);
    rect(cr, x - 6.0, y - 10.0, 12.0, 9.0, 0xb18b62);
    rect(cr, x - 8.0, y - 12.0, 16.0, 4.0, 0xd1aa77);
    for (dx, dy, w, h, shade) in [
        (-2.0, -33.0, 4.0, 23.0, 0x6a9d70),
        (-9.0, -28.0, 6.0, 12.0, 0x52805b),
        (4.0, -31.0, 5.0, 16.0, 0x7daa72),
        (-12.0, -23.0, 5.0, 8.0, 0x6b9c65),
        (7.0, -21.0, 5.0, 8.0, 0x52805b),
    ] {
        rect(cr, x + dx, y + dy, w, h, shade);
    }
}
fn workstation(cr: &Context, x: f64, y: f64, working: bool, frame: u32) {
    rect(cr, x - 26.0, y - 34.0, 55.0, 28.0, 0x624f40);
    for dx in [-24.0, 21.0] {
        rect(cr, x + dx, y - 26.0, 4.0, 23.0, 0x43414a);
    }
    rect(cr, x - 27.0, y - 37.0, 54.0, 24.0, 0xa77c50);
    rect(cr, x - 27.0, y - 37.0, 54.0, 2.0, 0xe0b87d);
    rect(cr, x - 25.0, y - 34.0, 50.0, 18.0, 0xc09765);
    rect(cr, x - 13.0, y - 55.0, 26.0, 21.0, 0x242f3c);
    rect(cr, x - 11.0, y - 53.0, 22.0, 17.0, 0xa6b0a8);
    rect(
        cr,
        x - 9.0,
        y - 51.0,
        18.0,
        12.0,
        if working { 0x283f47 } else { 0x516875 },
    );
    if working {
        for i in 0..3 {
            rect(
                cr,
                x - 7.0,
                y - 49.0 + i as f64 * 3.0,
                (6 + (frame as usize + i) % 7) as f64,
                1.0,
                0x8fca91,
            );
        }
    }
    rect(cr, x - 3.0, y - 34.0, 6.0, 3.0, 0x6c7880);
    rect(cr, x - 9.0, y - 31.0, 18.0, 3.0, 0xc7c9bc);
    rect(cr, x - 10.0, y - 25.0, 20.0, 5.0, 0x52606b);
    for i in 0..6 {
        rect(cr, x - 9.0 + i as f64 * 3.0, y - 24.0, 2.0, 2.0, 0xc5c9bf);
    }
    rect(cr, x + 17.0, y - 30.0, 5.0, 6.0, 0xf0dcb4);
    rect(cr, x - 10.0, y - 3.0, 20.0, 8.0, 0x263b41);
    rect(cr, x - 9.0, y - 5.0, 18.0, 7.0, 0x79a08a);
    rect(cr, x - 1.0, y + 5.0, 2.0, 5.0, 0x343942);
    rect(cr, x - 8.0, y + 9.0, 16.0, 2.0, 0x343942);
}
fn sofa(cr: &Context, x: f64, y: f64) {
    rect(cr, x - 20.0, y - 20.0, 41.0, 27.0, 0x253944);
    rect(cr, x - 19.0, y - 25.0, 38.0, 23.0, 0x905774);
    rect(cr, x - 17.0, y - 23.0, 34.0, 12.0, 0xb7758b);
    rect(cr, x - 16.0, y - 9.0, 32.0, 11.0, 0xc78999);
    for dx in [-21.0, 16.0] {
        rect(cr, x + dx, y - 15.0, 5.0, 19.0, 0x9d637d);
    }
}
fn draw_actor(cr: &Context, actor: &Actor, frame: u32) {
    let _ = cr.save();
    cr.translate(actor.position.0.round(), actor.position.1.round());
    let walking = !actor.path.is_empty();
    let beat = if walking || actor.status == AgentStatus::Working {
        (frame % 2) as f64
    } else {
        0.0
    };
    rect(cr, -10.0, -2.0, 20.0, 4.0, 0x4c4944);
    let species = actor.style % 12;
    let fur = FUR[species];
    let shirt = SHIRTS[actor.style / 12];
    for (x, y) in [(-6.0, -3.0 + beat), (2.0, -3.0 - beat)] {
        rect(cr, x, y, 5.0, 4.0, INK);
        rect(cr, x + 1.0, y, 3.0, 2.0, 0xbfc3c6);
    }
    rect(cr, -8.0, -16.0, 16.0, 13.0, INK);
    rect(cr, -6.0, -16.0, 12.0, 11.0, shirt);
    for i in 0..=actor.style / 12 % 3 {
        rect(cr, -4.0 + i as f64 * 3.0, -12.0, 1.0, 4.0, 0xf1ddbc);
    }
    rect(cr, -10.0, -13.0 - beat, 4.0, 6.0, fur);
    rect(cr, 6.0, -13.0 + beat, 4.0, 6.0, fur);
    // Different silhouettes as well as colors distinguish the twelve species.
    match species {
        0 | 3 => {
            for x in [-9.0, 4.0] {
                rect(cr, x, -32.0, 5.0, 9.0, INK);
                rect(cr, x + 1.0, -31.0, 3.0, 7.0, fur);
            }
        }
        1 => {
            for x in [-7.0, 3.0] {
                rect(cr, x, -38.0, 4.0, 14.0, INK);
                rect(cr, x + 1.0, -36.0, 2.0, 12.0, fur);
            }
        }
        8 => {
            for x in [-12.0, 7.0] {
                rect(cr, x, -26.0, 5.0, 16.0, 0x805c46);
            }
        }
        11 => {
            for x in [-14.0, 9.0] {
                for y in [-29.0, -24.0, -19.0] {
                    rect(cr, x, y, 5.0, 3.0, 0xc07da8);
                }
            }
        }
        6 | 7 => {}
        _ => {
            for x in [-11.0, 6.0] {
                rect(cr, x, -30.0, 6.0, 7.0, INK);
                rect(
                    cr,
                    x + 1.0,
                    -29.0,
                    4.0,
                    5.0,
                    if species == 4 { INK } else { fur },
                );
            }
        }
    }
    rect(cr, -9.0, -28.0, 18.0, 13.0, INK);
    rect(cr, -7.0, -30.0, 14.0, 18.0, INK);
    rect(cr, -8.0, -27.0, 16.0, 11.0, fur);
    rect(cr, -6.0, -29.0, 12.0, 15.0, fur);
    if matches!(species, 3 | 6 | 7) {
        rect(cr, -6.0, -23.0, 12.0, 8.0, 0xf1dfbf);
    }
    if species == 4 {
        for x in [-6.0, 2.0] {
            rect(cr, x, -25.0, 4.0, 5.0, INK);
        }
    }
    for x in [-5.0, 3.0] {
        rect(
            cr,
            x,
            -24.0,
            2.0,
            if matches!(actor.status, AgentStatus::Idle | AgentStatus::Done) && !walking {
                1.0
            } else {
                3.0
            },
            if species == 4 { 0xece5d2 } else { INK },
        );
        rect(cr, x - 1.0, -20.0, 3.0, 1.0, 0xd28d86);
    }
    rect(
        cr,
        -1.0,
        -20.0,
        2.0,
        2.0,
        if matches!(species, 6 | 7) {
            0xd1a94b
        } else {
            INK
        },
    );
    if species == 10 {
        rect(cr, -3.0, -20.0, 6.0, 3.0, 0xc78693);
    }
    if actor.style / 12 >= 5 {
        rect(cr, -9.0, -29.0, 18.0, 3.0, shirt);
        rect(cr, -5.0, -34.0, 10.0, 5.0, shirt);
    }
    let (symbol, color) = match actor.status {
        AgentStatus::Working => (if frame.is_multiple_of(2) { ".." } else { "..." }, 0xbad6b0),
        AgentStatus::Blocked => ("!", 0xf0c878),
        AgentStatus::Done => ("OK", 0xbad6b0),
        AgentStatus::Idle => ("z", 0xc8d3de),
        AgentStatus::Unknown => ("?", 0xc8d3de),
    };
    rect(cr, 7.0, -43.0, 20.0, 13.0, INK);
    rect(cr, 8.0, -42.0, 18.0, 11.0, color);
    rect(cr, 9.0, -31.0, 3.0, 3.0, color);
    text(cr, 11.0, -34.0, symbol, 8.0, INK);
    let _ = cr.restore();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    fn pixels(style: usize, status: AgentStatus, frame: u32) -> Vec<u8> {
        let mut surface =
            gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 360, 276).unwrap();
        {
            let cr = Context::new(&surface).unwrap();
            draw_room(
                &cr,
                276,
                &[Rc::new(RefCell::new(Actor::new(0, style, status)))],
                frame,
            );
            cr.status().unwrap();
        }
        let bytes = surface.data().unwrap().to_vec();
        bytes
    }
    #[test]
    fn agent_office_pixel_styles_and_states_are_distinct() {
        assert_eq!(
            (0..120)
                .map(|i| pixels(i, AgentStatus::Working, 0))
                .collect::<HashSet<_>>()
                .len(),
            120
        );
        assert_eq!(
            [
                AgentStatus::Working,
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Idle,
                AgentStatus::Unknown
            ]
            .map(|s| pixels(0, s, 0))
            .into_iter()
            .collect::<HashSet<_>>()
            .len(),
            5
        );
        assert_ne!(
            pixels(0, AgentStatus::Working, 0),
            pixels(0, AgentStatus::Working, 1)
        );
    }
    #[test]
    fn agent_office_walks_to_lounge_and_handles_interrupted_and_reduced_motion() {
        let mut actor = Actor::new(4, 17, AgentStatus::Working);
        actor.set_status(AgentStatus::Done);
        actor.advance(0.1, true);
        assert_ne!(actor.position, desk(4));
        assert_ne!(actor.position, lounge(4));
        for _ in 0..200 {
            actor.advance(0.1, true);
        }
        assert_eq!(actor.position, lounge(4));
        actor.set_status(AgentStatus::Working);
        actor.advance(0.1, true);
        actor.set_status(AgentStatus::Idle);
        for _ in 0..200 {
            actor.advance(0.1, true);
        }
        assert_eq!(actor.position, lounge(4));
        actor.set_status(AgentStatus::Blocked);
        actor.advance(0.0, false);
        assert_eq!(actor.position, desk(4));
        assert!(actor.path.is_empty());
        for slot in 0..100 {
            assert!(lounge(slot).1 + 40.0 < height(slot + 1) as f64);
        }
    }
}
