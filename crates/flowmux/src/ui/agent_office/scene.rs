// SPDX-License-Identifier: GPL-3.0-or-later
//! Original pixel characters and depth-sorted furniture for twenty-four floor plans.

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Facing {
    Up,
    Right,
    Down,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Walk,
    Type,
    Read,
    Sit,
    Wait,
    Coffee,
}

pub(super) struct Actor {
    pub slot: usize,
    style: usize,
    pub status: AgentStatus,
    pub position: (f64, f64),
    pub ended: Option<f64>,
    pub facing: Facing,
    pub reading: bool,
    pub status_age: f64,
    rest_phase: u8,
    rest_elapsed: f64,
    yielding: f64,
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
            facing: Facing::Down,
            reading: false,
            status_age: 0.0,
            rest_phase: 0,
            rest_elapsed: 0.0,
            yielding: 0.0,
            plan,
            path: VecDeque::new(),
        }
    }
    pub fn replan(&mut self, slot: usize, plan: Rc<Plan>) {
        self.slot = slot;
        // Preserve the actor's position within the room instead of teleporting to its seat.
        self.position.0 *= plan.width / self.plan.width;
        self.position.1 *= plan.height / self.plan.height;
        self.plan = plan;
        self.path = self.plan.route(self.position, self.destination());
    }
    fn destination(&self) -> (f64, f64) {
        if self.ended.is_none()
            && matches!(self.status, AgentStatus::Idle | AgentStatus::Done)
            && self.rest_phase > 0
        {
            self.plan.rest_stop(self.slot, self.rest_phase)
        } else {
            self.plan
                .destination(self.slot, self.status, self.ended.is_some())
        }
    }

    pub fn set_status(&mut self, status: AgentStatus) {
        if self.status == status && self.ended.is_none() {
            return;
        }
        self.status = status;
        self.status_age = 0.0;
        self.rest_phase = 0;
        self.rest_elapsed = 0.0;
        self.yielding = 0.0;
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
        self.ended.is_some_and(|age| age >= 4.0)
            && self.path.is_empty()
            && self.position == self.plan.destination(self.slot, self.status, true)
    }
    pub fn action(&self) -> Action {
        if !self.path.is_empty() {
            Action::Walk
        } else {
            match self.status {
                AgentStatus::Working if self.reading => Action::Read,
                AgentStatus::Working => Action::Type,
                AgentStatus::Done | AgentStatus::Idle if self.rest_phase == 1 => Action::Coffee,
                AgentStatus::Done | AgentStatus::Idle if self.rest_phase == 2 => Action::Wait,
                AgentStatus::Done => Action::Sit,
                AgentStatus::Idle => Action::Wait,
                _ => Action::Wait,
            }
        }
    }
    pub fn yield_to(&mut self, dt: f64, neighbors: impl Iterator<Item = (f64, f64)>) -> bool {
        let Some(&next) = self.path.front() else {
            self.yielding = 0.0;
            return false;
        };
        if self.yielding < 0.0 {
            self.yielding = (self.yielding + dt).min(0.0);
            return false;
        }
        let delta = (next.0 - self.position.0, next.1 - self.position.1);
        let length = delta.0.hypot(delta.1);
        if length < 0.001 {
            return false;
        }
        let ratio = (dt * 120.0 / length).min(1.0);
        let proposed = (
            self.position.0 + delta.0 * ratio,
            self.position.1 + delta.1 * ratio,
        );
        if neighbors.into_iter().any(|other| {
            let distance = (other.0 - proposed.0).hypot(other.1 - proposed.1);
            distance < 16.0
                && distance < (other.0 - self.position.0).hypot(other.1 - self.position.1)
        }) {
            self.yielding += dt;
            if self.yielding < 0.3 + (self.slot % 4) as f64 * 0.05 {
                return true;
            }
            // Bounded yielding keeps opposing walkers from deadlocking in a narrow aisle.
            self.yielding = -0.3;
        } else {
            self.yielding = 0.0;
        }
        false
    }
    pub fn advance(&mut self, dt: f64, animate: bool) -> bool {
        self.status_age += dt;
        if let Some(age) = self.ended.as_mut() {
            *age += dt;
        }
        if !animate {
            let changed = !self.path.is_empty();
            self.position = self.destination();
            self.path.clear();
            return changed;
        }
        if self.path.is_empty()
            && self.ended.is_none()
            && matches!(self.status, AgentStatus::Idle | AgentStatus::Done)
        {
            self.rest_elapsed += dt;
            let pause = if self.rest_phase == 0 {
                12.0 + (self.slot % 7) as f64
            } else {
                5.0
            };
            if self.rest_elapsed >= pause {
                self.rest_elapsed = 0.0;
                self.rest_phase = (self.rest_phase + 1) % 3;
                self.path = self.plan.route(self.position, self.destination());
            }
        }
        let mut distance = dt * 120.0;
        let moving = !self.path.is_empty();
        while let Some(&(x, y)) = self.path.front() {
            let (dx, dy) = (x - self.position.0, y - self.position.1);
            let length = dx.hypot(dy);
            if length > 0.001 {
                self.facing = if dx.abs() > dy.abs() {
                    if dx > 0.0 {
                        Facing::Right
                    } else {
                        Facing::Left
                    }
                } else if dy > 0.0 {
                    Facing::Down
                } else {
                    Facing::Up
                };
            }
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
        super::layout::TEMPLATES[design].name
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
    for zone in &plan.rest {
        rect(cr, zone.x, zone.y, zone.w, zone.h, rug);
        for x in [zone.x + 3.0, zone.x + zone.w - 4.0] {
            rect(cr, x, zone.y + 3.0, 1.0, zone.h - 6.0, accent);
        }
        text(cr, zone.x + 6.0, zone.y + 10.0, "LOUNGE", 7.0, accent);
    }
    for zone in &plan.work {
        text(
            cr,
            zone.x + 6.0,
            zone.y + 10.0,
            "WORK / REVIEW",
            7.0,
            accent,
        );
    }
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
    enum Object<'a> {
        Desk(usize),
        Sofa(usize),
        Table(usize),
        Plant(f64, f64),
        Actor(&'a Actor),
    }
    let mut objects = Vec::new();
    for slot in 0..plan.capacity {
        objects.push((plan.desk(slot).1 - 16.0, Object::Desk(slot)));
    }
    for (slot, &(_, y)) in plan.benches.iter().enumerate() {
        objects.push((y - 8.0, Object::Sofa(slot)));
        let table = plan.coffee_table(slot);
        objects.push((table.y + table.h, Object::Table(slot)));
    }
    for (x, y) in plan.plants() {
        objects.push((y, Object::Plant(x, y)));
    }
    for actor in actors {
        objects.push((actor.position.1, Object::Actor(actor)));
    }
    objects.sort_by(|a, b| a.0.total_cmp(&b.0));
    text(cr, w / 2.0 - 16.0, h - 13.0, "EXIT", 7.0, accent);
    for (_, object) in objects {
        match object {
            Object::Desk(slot) => {
                let (x, y) = plan.desk(slot);
                let active = actors.iter().any(|a| {
                    a.slot == slot && a.status == AgentStatus::Working && a.ended.is_none()
                });
                workstation(cr, x, y, active, frame);
                rect(cr, x + 28.0, y - 4.0, 12.0, 12.0, accent);
            }
            Object::Sofa(slot) => {
                let (x, y) = plan.benches[slot];
                let _ = cr.save();
                cr.translate(x, y);
                cr.scale(2.0, 1.0);
                sofa(cr, 0.0, 0.0);
                let _ = cr.restore();
            }
            Object::Table(slot) => {
                let table = plan.coffee_table(slot);
                rect(
                    cr,
                    table.x + 2.0,
                    table.y + 3.0,
                    table.w - 4.0,
                    table.h - 3.0,
                    0x514b44,
                );
                rect(cr, table.x, table.y, table.w, 6.0, accent);
                rect(cr, table.x + 5.0, table.y - 5.0, 5.0, 5.0, 0xf0dfba);
            }
            Object::Plant(x, y) => plant(cr, x, y),
            Object::Actor(actor) => draw_actor(cr, actor, frame),
        }
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
    let action = actor.action();
    let walking = action == Action::Walk;
    let facing = match action {
        Action::Type => Facing::Up,
        Action::Walk => actor.facing,
        _ => Facing::Down,
    };
    if facing == Facing::Left {
        cr.scale(-1.0, 1.0);
    }
    if matches!(facing, Facing::Left | Facing::Right) {
        cr.scale(0.8, 1.0);
    }
    let seated = matches!(action, Action::Type | Action::Read | Action::Sit);
    let beat = if walking || action == Action::Type {
        (frame % 2) as f64
    } else {
        0.0
    };
    rect(cr, -10.0, -2.0, 20.0, 4.0, 0x4c4944);
    let species = actor.style % 12;
    let fur = FUR[species];
    let shirt = SHIRTS[actor.style / 12];
    let step = if walking { beat * 2.0 } else { 0.0 };
    for (x, y) in [(-6.0, -3.0 + step), (2.0, -3.0 - step)] {
        let y = if seated { y - 3.0 } else { y };
        rect(cr, x, y, 5.0, 4.0, INK);
        rect(cr, x + 1.0, y, 3.0, 2.0, 0xbfc3c6);
    }
    rect(cr, -8.0, -16.0, 16.0, 13.0, INK);
    rect(cr, -6.0, -16.0, 12.0, 11.0, shirt);
    for i in 0..=actor.style / 12 % 3 {
        rect(cr, -4.0 + i as f64 * 3.0, -12.0, 1.0, 4.0, 0xf1ddbc);
    }
    let hands_y = if action == Action::Type { -22.0 } else { -13.0 };
    rect(cr, -10.0, hands_y - beat, 4.0, 6.0, fur);
    rect(cr, 6.0, hands_y + beat, 4.0, 6.0, fur);
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
    if facing != Facing::Up {
        if matches!(species, 3 | 6 | 7) {
            rect(cr, -6.0, -23.0, 12.0, 8.0, 0xf1dfbf);
        }
        if species == 4 {
            for x in [-6.0, 2.0] {
                rect(cr, x, -25.0, 4.0, 5.0, INK);
            }
        }
        let eyes: &[f64] = if matches!(facing, Facing::Left | Facing::Right) {
            &[4.0]
        } else {
            &[-5.0, 3.0]
        };
        for &x in eyes {
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
    }
    if action == Action::Coffee {
        let lift = if frame % 6 < 2 { 4.0 } else { 0.0 };
        rect(cr, 6.0, -15.0 - lift, 6.0, 7.0, 0xf0dfba);
        rect(cr, 12.0, -14.0 - lift, 2.0, 4.0, 0xf0dfba);
    }
    if action == Action::Read {
        rect(cr, -9.0, -17.0, 18.0, 10.0, 0x526b91);
        rect(cr, -7.0, -16.0, 14.0, 7.0, 0xf0dfba);
        rect(cr, (frame % 2) as f64 - 1.0, -16.0, 1.0, 8.0, 0x887968);
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
    fn desk_occludes_a_character_walking_behind_it() {
        let plan = Rc::new(Plan::new(2, 0));
        let (x, y) = plan.desk(0);
        let sample = |with_actor| {
            let mut surface = gtk::cairo::ImageSurface::create(
                gtk::cairo::Format::ARgb32,
                plan.width as i32,
                plan.height as i32,
            )
            .unwrap();
            {
                let cr = Context::new(&surface).unwrap();
                let mut actor = Actor::new(0, 0, AgentStatus::Idle, plan.clone());
                actor.position = (x, y - 20.0);
                let actors = [&actor];
                draw_room(&cr, &plan, 0, if with_actor { &actors } else { &[] }, 0);
            }
            let stride = surface.stride() as usize;
            let offset = (y as usize - 28) * stride + x as usize * 4;
            let pixel = surface.data().unwrap()[offset..offset + 4].to_vec();
            pixel
        };
        assert_eq!(sample(true), sample(false));
    }

    #[test]
    fn rest_actions_return_to_work_and_crowd_yield_is_bounded() {
        let plan = Rc::new(Plan::new(3, 0));
        let mut actor = Actor::new(0, 0, AgentStatus::Done, plan.clone());
        let mut coffee = false;
        let mut stroll = false;
        let mut returned = false;
        for _ in 0..1000 {
            actor.advance(0.1, true);
            coffee |= actor.action() == Action::Coffee;
            stroll |= actor.rest_phase == 2;
            returned |= stroll && actor.action() == Action::Sit;
            if returned {
                break;
            }
        }
        assert!(coffee && stroll && returned);
        actor.set_status(AgentStatus::Working);
        assert_eq!(actor.rest_phase, 0);
        for _ in 0..500 {
            actor.advance(0.1, true);
            if actor.path.is_empty() {
                break;
            }
        }
        assert_eq!(actor.position, plan.desk(0));
        assert_eq!(actor.action(), Action::Type);
        actor.reading = true;
        assert_eq!(actor.action(), Action::Read);
        actor.set_status(AgentStatus::Done);
        actor.advance(0.01, true);
        let neighbor = *actor.path.front().unwrap();
        assert!(actor.yield_to(0.1, std::iter::once(neighbor)));
        assert!((0..8).any(|_| !actor.yield_to(0.1, std::iter::once(neighbor))));
        let mut still = Actor::new(0, 0, AgentStatus::Done, plan);
        for _ in 0..500 {
            still.advance(0.1, false);
        }
        assert_eq!(
            still.rest_phase, 0,
            "reduced motion disables ambient wandering"
        );
    }

    #[test]
    fn walking_renders_four_directions_and_reading_has_a_separate_pose() {
        let plan = Rc::new(Plan::new(2, 0));
        let render = |facing, reading, walking| {
            let mut surface =
                gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, 80, 80).unwrap();
            {
                let cr = Context::new(&surface).unwrap();
                let mut actor = Actor::new(0, 0, AgentStatus::Working, plan.clone());
                actor.position = (40.0, 60.0);
                actor.facing = facing;
                actor.reading = reading;
                if walking {
                    actor.path.push_back((48.0, 60.0));
                }
                draw_actor(&cr, &actor, 0);
            }
            let pixels = surface.data().unwrap().to_vec();
            pixels
        };
        assert_eq!(
            [Facing::Up, Facing::Down, Facing::Left, Facing::Right]
                .into_iter()
                .map(|f| render(f, false, true))
                .collect::<HashSet<_>>()
                .len(),
            4
        );
        assert_ne!(
            render(Facing::Up, false, false),
            render(Facing::Down, true, false)
        );
    }

    #[test]
    fn reflow_preserves_relative_position_and_continues_walking() {
        let plan = Rc::new(Plan::fit(2, 0, 1.3));
        let mut actor = Actor::new(0, 0, AgentStatus::Working, plan.clone());
        actor.set_status(AgentStatus::Done);
        actor.advance(0.1, true);
        let before = (
            actor.position.0 / plan.width,
            actor.position.1 / plan.height,
        );
        let next = Rc::new(Plan::fit(3, 0, 1.31));
        actor.replan(0, next.clone());
        assert!((actor.position.0 / next.width - before.0).abs() < 0.000_001);
        assert!((actor.position.1 / next.height - before.1).abs() < 0.000_001);
        assert_ne!(actor.position, next.sofa(0));
        for _ in 0..400 {
            actor.advance(0.1, true);
            if actor.path.is_empty() {
                break;
            }
        }
        assert_eq!(actor.position, next.sofa(0));
        actor.finish();
        actor.path.clear();
        actor.advance(5.0, true);
        assert!(
            !actor.departed(),
            "an unreachable exit must not remove the actor"
        );
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
                    if actor.path.is_empty() {
                        break;
                    }
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
