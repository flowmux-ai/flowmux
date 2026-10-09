// SPDX-License-Identifier: GPL-3.0-or-later
//! Original pixel artwork: six furnished themes × four spatial plans.

use super::layout::Plan;
use flowmux_core::AgentStatus;
use gtk::cairo::Context;
use std::{collections::VecDeque, rc::Rc};

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

pub(super) struct Actor {
    pub slot: usize,
    style: usize,
    pub status: AgentStatus,
    pub position: (f64, f64),
    pub ended: Option<f64>,
    plan: Rc<Plan>,
    path: VecDeque<(f64, f64)>,
}

impl Actor {
    pub fn new(slot: usize, style: usize, status: AgentStatus, plan: Rc<Plan>) -> Self {
        Self {
            slot,
            style,
            status,
            position: plan.destination(slot, status, false),
            ended: None,
            plan,
            path: VecDeque::new(),
        }
    }
    pub fn replan(&mut self, slot: usize, plan: Rc<Plan>) {
        self.slot = slot;
        self.position = plan.destination(slot, self.status, self.ended.is_some());
        self.path.clear();
        self.plan = plan;
    }
    pub fn set_status(&mut self, status: AgentStatus) {
        if self.status == status && self.ended.is_none() {
            return;
        }
        self.status = status;
        self.ended = None;
        self.path = self.plan.route(
            self.position,
            self.plan.destination(self.slot, status, false),
        );
    }
    pub fn finish(&mut self) {
        if self.ended.is_none() {
            self.ended = Some(0.0);
            self.path = self.plan.route(
                self.position,
                self.plan.destination(self.slot, self.status, true),
            );
        }
    }
    pub fn departed(&self) -> bool {
        self.ended.is_some_and(|age| age >= 4.0) && self.path.is_empty()
    }
    pub fn advance(&mut self, dt: f64, animate: bool) -> bool {
        if let Some(age) = self.ended.as_mut() {
            *age += dt;
        }
        if !animate {
            let changed = !self.path.is_empty();
            self.position = self
                .plan
                .destination(self.slot, self.status, self.ended.is_some());
            self.path.clear();
            return changed;
        }
        let mut distance = dt * 120.0;
        let moving = !self.path.is_empty();
        while let Some(&(x, y)) = self.path.front() {
            let (dx, dy) = (x - self.position.0, y - self.position.1);
            let length = dx.hypot(dy);
            if length > distance {
                self.position.0 += dx / length * distance;
                self.position.1 += dy / length * distance;
                break;
            }
            self.position = (x, y);
            distance -= length;
            self.path.pop_front();
        }
        moving
    }
}

pub(super) const THEMES: [&str; 6] = [
    "Maple studio",
    "Botanical lab",
    "Harbor loft",
    "Midnight arcade",
    "Rose library",
    "Desert workshop",
];
pub(super) fn design_name(design: usize) -> String {
    format!(
        "{} / {}",
        THEMES[design / 4],
        ["West wing", "East wing", "North wing", "South wing"][design % 4]
    )
}

pub(super) fn draw_room(cr: &Context, plan: &Plan, design: usize, actors: &[&Actor], frame: u32) {
    let theme = design / 4;
    let (wall, floor, seam, rug, accent) = [
        (0x39465a, 0x8d6546, 0x78563e, 0x466775, 0xd1ab79),
        (0x36554b, 0xb0a889, 0x929a7b, 0x627f59, 0xbad19a),
        (0x42677d, 0xd4c0a0, 0xbda88c, 0x578d9d, 0xe8d7aa),
        (0x3a3258, 0x55516f, 0x47425f, 0x72456e, 0xb694dc),
        (0x634655, 0xa47c6c, 0x8e695e, 0x896e84, 0xe0b6ac),
        (0x755849, 0xc6a16c, 0xae895c, 0x6c817a, 0xe6ca90),
    ][theme];
    let (w, h) = (plan.width, plan.height);
    cr.set_antialias(gtk::cairo::Antialias::None);
    rect(cr, 0.0, 0.0, w, h, 0x17212b);
    rect(cr, 2.0, 2.0, w - 4.0, h - 4.0, floor);
    for row in 0..((h - 56.0) / 16.0) as usize {
        let y = 56.0 + row as f64 * 16.0;
        rect(cr, 8.0, y, w - 16.0, 1.0, seam);
        for col in 0..(w / 48.0) as usize {
            let x = 8.0 + col as f64 * 48.0 + (row % 2) as f64 * 24.0;
            if x < w - 8.0 {
                rect(cr, x, y, 1.0, 16.0, seam);
            }
        }
    }
    rect(cr, 2.0, 2.0, w - 4.0, 54.0, wall);
    rect(cr, 8.0, 53.0, w - 16.0, 3.0, 0x25313e);
    rect(cr, plan.rest.x, plan.rest.y, plan.rest.w, plan.rest.h, rug);
    for x in [plan.rest.x + 3.0, plan.rest.x + plan.rest.w - 4.0] {
        rect(cr, x, plan.rest.y + 3.0, 1.0, plan.rest.h - 6.0, accent);
    }
    text(
        cr,
        plan.work.x + 6.0,
        plan.work.y + 10.0,
        "WORK / REVIEW",
        7.0,
        accent,
    );
    text(
        cr,
        plan.rest.x + 6.0,
        plan.rest.y + 10.0,
        "LOUNGE / COFFEE",
        7.0,
        accent,
    );
    bookshelf(cr, 24.0, 17.0);
    rect(cr, w / 2.0 - 27.0, 14.0, 54.0, 31.0, 0x202a39);
    rect(
        cr,
        w / 2.0 - 24.0,
        17.0,
        48.0,
        25.0,
        if theme == 3 { 0x7584b7 } else { 0x8ec3c5 },
    );
    rect(cr, w / 2.0 - 1.0, 17.0, 2.0, 25.0, accent);
    rect(cr, w / 2.0 - 24.0, 29.0, 48.0, 2.0, accent);
    // Each theme changes furnishings as well as the palette.
    match theme {
        0 | 4 => {
            bookshelf(cr, w - 76.0, 17.0);
            if theme == 4 {
                bookshelf(cr, 90.0, 17.0);
            }
        }
        1 => {
            for x in [w - 36.0, w - 66.0, 106.0] {
                plant(cr, x, 47.0);
            }
        }
        2 => {
            rect(cr, w - 72.0, 18.0, 48.0, 26.0, 0xd5bb87);
            rect(cr, w - 69.0, 21.0, 42.0, 20.0, 0x74a6b0);
            text(cr, w - 64.0, 35.0, "~ ~ ~", 8.0, 0xdcedd8);
        }
        3 => {
            for x in [w - 72.0, w - 45.0] {
                rect(cr, x, 13.0, 22.0, 32.0, 0x27283e);
                rect(cr, x + 3.0, 17.0, 16.0, 14.0, 0xa470b5);
                text(cr, x + 5.0, 27.0, ">_", 8.0, 0xefc684);
                rect(cr, x + 4.0, 35.0, 14.0, 3.0, 0x829ccd);
            }
        }
        _ => {
            rect(cr, w - 78.0, 15.0, 56.0, 30.0, 0x3d494a);
            for x in [w - 70.0, w - 54.0, w - 38.0] {
                rect(cr, x, 21.0, 3.0, 17.0, accent);
                rect(cr, x - 3.0, 20.0, 9.0, 4.0, 0xa9b7b3);
            }
        }
    }
    for slot in 0..plan.capacity {
        let (x, y) = plan.desk(slot);
        let active = actors
            .iter()
            .any(|a| a.slot == slot && a.status == AgentStatus::Working && a.ended.is_none());
        workstation(cr, x, y, active, frame);
        // Review pad sits beside the desk; the standing position is below it.
        rect(cr, x + 28.0, y - 4.0, 12.0, 12.0, accent);
        let (x, y) = plan.sofa(slot);
        sofa(cr, x, y);
        rect(cr, x + 28.0, y - 9.0, 13.0, 16.0, 0x514b44);
        rect(cr, x + 26.0, y - 12.0, 17.0, 6.0, accent);
        rect(cr, x + 31.0, y - 17.0, 5.0, 5.0, 0xf0dfba);
    }
    plant(cr, 26.0, h - 10.0);
    plant(cr, w - 26.0, h - 10.0);
    text(cr, w / 2.0 - 16.0, h - 13.0, "EXIT", 7.0, accent);
    let mut sorted = actors.to_vec();
    sorted.sort_by(|a, b| a.position.1.total_cmp(&b.position.1));
    for actor in sorted {
        draw_actor(cr, actor, frame);
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
    rect(
        cr,
        -2.0,
        -8.0,
        4.0,
        2.0,
        match actor.status {
            AgentStatus::Working => 0x90ca9b,
            AgentStatus::Blocked => 0xf0c878,
            AgentStatus::Done => 0x91bbef,
            AgentStatus::Idle => 0xb49dcd,
            AgentStatus::Unknown => 0xeeeeee,
        },
    );
    let _ = cr.restore();
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    fn pixels(style: usize, status: AgentStatus, frame: u32, design: usize) -> Vec<u8> {
        let plan = Rc::new(Plan::new(2, design));
        let mut surface = gtk::cairo::ImageSurface::create(
            gtk::cairo::Format::ARgb32,
            plan.width as i32,
            plan.height as i32,
        )
        .unwrap();
        {
            let cr = Context::new(&surface).unwrap();
            let actor = Actor::new(0, style, status, plan.clone());
            draw_room(&cr, &plan, design, &[&actor], frame);
            cr.status().unwrap();
        }
        let bytes = surface.data().unwrap().to_vec();
        bytes
    }
    #[test]
    fn agent_office_has_120_characters_and_24_distinct_furnished_designs() {
        assert_eq!(
            (0..120)
                .map(|i| pixels(i, AgentStatus::Working, 0, 0))
                .collect::<HashSet<_>>()
                .len(),
            120
        );
        assert_eq!(
            (0..24)
                .map(|i| pixels(0, AgentStatus::Working, 0, i))
                .collect::<HashSet<_>>()
                .len(),
            24
        );
        assert_eq!((0..24).map(design_name).collect::<HashSet<_>>().len(), 24);
        assert_ne!(
            pixels(0, AgentStatus::Working, 0, 0),
            pixels(0, AgentStatus::Working, 1, 0)
        );
    }
    #[test]
    fn agent_office_routes_every_state_and_interruption_and_honors_reduced_motion() {
        for design in 0..24 {
            let plan = Rc::new(Plan::new(8, design));
            let mut actor = Actor::new(4, 17, AgentStatus::Working, plan.clone());
            for status in [
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Idle,
                AgentStatus::Working,
            ] {
                actor.set_status(status);
                actor.advance(0.1, true);
                assert_ne!(actor.position, plan.destination(4, status, false));
                for _ in 0..200 {
                    actor.advance(0.1, true);
                }
                assert_eq!(actor.position, plan.destination(4, status, false));
            }
            actor.set_status(AgentStatus::Done);
            actor.advance(0.3, true);
            actor.set_status(AgentStatus::Blocked);
            for _ in 0..200 {
                actor.advance(0.1, true);
            }
            assert_eq!(
                actor.position,
                plan.destination(4, AgentStatus::Blocked, false)
            );
            actor.set_status(AgentStatus::Idle);
            actor.advance(0.0, false);
            assert_eq!(
                actor.position,
                plan.destination(4, AgentStatus::Idle, false)
            );
            actor.finish();
            for _ in 0..200 {
                actor.advance(0.1, true);
            }
            assert_eq!(actor.position, plan.destination(4, AgentStatus::Idle, true));
        }
    }
}
