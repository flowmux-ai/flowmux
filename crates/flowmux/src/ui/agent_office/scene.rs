// SPDX-License-Identifier: GPL-3.0-or-later
//! Original pixel characters and depth-sorted furniture for twenty-four floor plans.

use super::{
    art,
    layout::{Plan, OBJECT_SCALE},
};
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
        let settled = self.path.is_empty() && self.position == self.destination();
        self.slot = slot;
        // Moving actors retain their relative position; settled actors stay with their furniture.
        self.position.0 *= plan.width / self.plan.width;
        self.position.1 *= plan.height / self.plan.height;
        self.plan = plan;
        if settled {
            self.position = self.destination();
        } else {
            self.path = self.plan.route(self.position, self.destination());
        }
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
    "Daylight atelier",
    "Fern conservatory",
    "Signal observatory",
    "Folio library",
    "Copper workshop",
    "Tea commons",
];
pub(super) fn design_name(design: usize) -> String {
    format!(
        "{} / {}",
        THEMES[design / 4],
        super::layout::TEMPLATES[design].name
    )
}

pub(super) struct Background {
    surface: gtk::cairo::ImageSurface,
    pub scale: f64,
}

impl Background {
    pub fn new(plan: &Plan, design: usize, scale: f64) -> Result<Self, gtk::cairo::Error> {
        let surface = gtk::cairo::ImageSurface::create(
            gtk::cairo::Format::ARgb32,
            (plan.width * scale).ceil() as i32,
            (plan.height * scale).ceil() as i32,
        )?;
        let cr = Context::new(&surface)?;
        cr.set_antialias(gtk::cairo::Antialias::None);
        cr.scale(scale, scale);
        art::shell(&cr, plan, design);
        cr.status()?;
        Ok(Self { surface, scale })
    }

    fn paint(&self, cr: &Context) {
        let _ = cr.save();
        cr.scale(1.0 / self.scale, 1.0 / self.scale);
        let _ = cr.set_source_surface(&self.surface, 0., 0.);
        cr.source().set_filter(gtk::cairo::Filter::Nearest);
        let _ = cr.paint();
        let _ = cr.restore();
    }
}

pub(super) fn draw_room(
    cr: &Context,
    plan: &Plan,
    design: usize,
    actors: &[&Actor],
    frame: u32,
    background: Option<&Background>,
) {
    let theme = design / 4;
    let accent = art::PALETTES[theme].accent;
    let (w, h) = (plan.width, plan.height);
    cr.set_antialias(gtk::cairo::Antialias::None);
    if let Some(background) = background {
        background.paint(cr);
    } else {
        art::shell(cr, plan, design);
    }
    enum Object<'a> {
        Desk(usize),
        Sofa(usize),
        Chair(usize),
        Fixture(usize),
        Table(usize),
        Plant(f64, f64),
        Actor(&'a Actor),
    }
    let mut objects = Vec::new();
    for slot in 0..plan.capacity {
        let y = plan.desk(slot).1;
        objects.push((
            y + if plan.desk_faces_south(slot) {
                64.0
            } else {
                -16.0
            } * OBJECT_SCALE,
            Object::Desk(slot),
        ));
        objects.push((y - 1.0, Object::Chair(slot)));
    }
    for (slot, &(_, y)) in plan.benches.iter().enumerate() {
        objects.push((y - 8.0 * OBJECT_SCALE, Object::Sofa(slot)));
        let table = plan.coffee_table(slot).enlarged(plan.benches[slot]);
        objects.push((table.y + table.h, Object::Table(slot)));
    }
    for (slot, area) in plan.fixtures.iter().enumerate() {
        objects.push((area.y + area.h, Object::Fixture(slot)));
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
        let origin = match object {
            Object::Desk(slot) | Object::Chair(slot) => plan.desk(slot),
            Object::Sofa(slot) | Object::Table(slot) => plan.benches[slot],
            Object::Fixture(slot) => {
                let r = plan.fixtures[slot];
                (r.x + r.w / 2., r.y + r.h)
            }
            Object::Plant(x, y) => (x, y),
            Object::Actor(actor) => actor.position,
        };
        let _ = cr.save();
        cr.translate(origin.0, origin.1);
        cr.scale(OBJECT_SCALE, OBJECT_SCALE);
        cr.translate(-origin.0, -origin.1);
        match object {
            Object::Desk(slot) => {
                let (x, y) = plan.desk(slot);
                let active = actors.iter().any(|a| {
                    a.slot == slot && a.status == AgentStatus::Working && a.ended.is_none()
                });
                workstation(
                    cr,
                    (x, y),
                    theme,
                    slot,
                    plan.desk_faces_south(slot),
                    active,
                    frame,
                );
                rect(cr, x + 28.0, y - 4.0, 12.0, 12.0, accent);
            }
            Object::Sofa(slot) => {
                let (x, y) = plan.benches[slot];
                sofa(cr, x, y, theme, (design + slot) % 4);
            }
            Object::Chair(slot) => {
                let (x, y) = plan.desk(slot);
                rect(cr, x - 1., y - 2., 2., 10., 0x34414b);
                rect(cr, x - 10., y + 7., 20., 3., 0x34414b);
                rect(cr, x - 12., y - 9., 24., 12., 0x293844);
                rect(cr, x - 10., y - 9., 20., 9., art::PALETTES[theme].seat);
            }
            Object::Fixture(slot) => {
                art::fixture(cr, plan.fixtures[slot], theme, design % 4, frame)
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
        let _ = cr.restore();
    }
}

pub(super) fn rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, color: u32) {
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
fn workstation(
    cr: &Context,
    position: (f64, f64),
    theme: usize,
    slot: usize,
    south: bool,
    working: bool,
    frame: u32,
) {
    let (x, y) = position;
    let p = &art::PALETTES[theme];
    let top = y + if south { 4. } else { -48. };
    // A front and a rear view share the same footprint, with upright monitors in both.
    let _ = cr.save();
    cr.set_source_rgba(0.20, 0.18, 0.14, 0.16);
    cr.rectangle(x - 31., top + 14., 69., 32.);
    let _ = cr.fill();
    let _ = cr.restore();
    rect(cr, x - 31., top + 7., 62., 30., p.wall);
    for dx in [-29., 26.] {
        rect(cr, x + dx, top + 20., 4., 27., 0x34414b);
    }
    rect(cr, x - 34., top + 2., 68., 26., p.wood);
    rect(cr, x - 32., top, 64., 30., p.wood);
    rect(cr, x - 32., top, 64., 2., p.accent);
    rect(cr, x - 32., top + 28., 64., 3., p.seam);
    rect(cr, x - 32., top + 31., 64., 2., p.wall);
    for (dx, dy, width) in [(-29., 24., 14.), (17., 4., 11.), (-30., 4., 9.)] {
        rect(cr, x + dx, top + dy, width, 1., p.seam);
    }
    if theme == 4 {
        rect(cr, x + 20., top + 32., 12., 12., p.wood);
        rect(cr, x + 23., top + 36., 6., 2., p.accent);
    }
    let screen = top - if south { 8. } else { 20. };
    rect(cr, x - 15., screen, 30., 24., 0x25333e);
    rect(
        cr,
        x - 12.,
        screen + 3.,
        24.,
        17.,
        if south { p.wall } else { 0x41616d },
    );
    if south {
        rect(cr, x - 8., screen + 7., 16., 2., p.seam);
        rect(cr, x - 2., screen + 13., 4., 4., p.accent);
    } else if working {
        for i in 0..4 {
            rect(
                cr,
                x - 9.,
                screen + 5. + i as f64 * 3.,
                (8 + (frame as usize + i) % 9) as f64,
                1.,
                p.accent,
            );
        }
    } else {
        rect(cr, x - 4., screen + 8., 8., 7., p.seat);
    }
    rect(cr, x - 2., screen + 24., 4., 4., 0x677984);
    rect(cr, x - 8., screen + 27., 16., 2., 0x34414b);
    if !south {
        rect(cr, x - 12., top + 17., 24., 7., 0x43505a);
        for i in 0..7 {
            rect(cr, x - 10. + i as f64 * 3., top + 19., 2., 2., 0xc6d1cb);
        }
    }
    match (slot + theme) % 3 {
        0 => {
            rect(cr, x + 23., top + 9., 6., 8., 0xeee0bd);
            rect(cr, x + 29., top + 10., 3., 5., p.accent);
            rect(cr, x + 24., top + 9., 4., 2., 0x735944);
        }
        1 => {
            rect(cr, x + 21., top + 12., 9., 7., p.seat);
            rect(cr, x + 23., top + 3., 5., 10., 0x557e63);
            rect(cr, x + 19., top + 6., 12., 4., 0xa4c68c);
        }
        _ => {
            rect(cr, x + 19., top + 10., 11., 9., p.rug);
            rect(cr, x + 20., top + 8., 11., 8., 0xefdbb9);
            rect(cr, x + 22., top + 10., 7., 1., p.seat);
        }
    }
    match theme {
        0 => {
            rect(cr, x - 31., top + 8., 13., 14., p.rug);
            rect(cr, x - 29., top + 6., 10., 13., 0xf0dcbb);
            rect(cr, x - 27., top + 8., 6., 2., p.seat);
            rect(cr, x - 27., top + 12., 4., 3., p.rug);
        }
        1 => {
            rect(cr, x - 31., top + 8., 13., 11., p.seat);
            rect(cr, x - 27., top - 7., 3., 17., 0x476d53);
            rect(cr, x - 32., top - 5., 14., 5., 0xa6c586);
            rect(cr, x - 30., top + 2., 10., 4., 0x7da575);
        }
        2 => {
            rect(cr, x - 32., top - 20., 14., 26., 0x25333e);
            rect(cr, x - 30., top - 18., 10., 21., 0x35566c);
            for i in 0..3 {
                rect(cr, x - 28., top - 15. + i as f64 * 5., 6., 2., p.accent);
            }
            rect(cr, x - 28., top + 6., 6., 3., 0x34414b);
        }
        3 => {
            rect(cr, x - 26., top - 18., 2., 31., 0x34414b);
            rect(cr, x - 33., top - 20., 16., 7., p.seat);
            rect(cr, x - 31., top - 13., 12., 2., p.accent);
            rect(cr, x - 30., top + 12., 10., 3., 0x34414b);
            rect(cr, x - 30., top + 18., 12., 5., p.rug);
        }
        4 => {
            rect(cr, x - 30., top + 4., 10., 14., p.wall);
            for i in 0..3 {
                rect(
                    cr,
                    x - 29. + i as f64 * 3.,
                    top - 5. + i as f64 * 2.,
                    2.,
                    11.,
                    p.accent,
                );
            }
            rect(cr, x - 31., top + 23., 20., 2., 0xf0d6ac);
        }
        _ => {
            rect(cr, x - 32., top + 17., 16., 4., p.rug);
            rect(cr, x - 30., top + 7., 12., 11., 0xece0c4);
            rect(cr, x - 29., top + 4., 10., 3., p.seat);
            rect(cr, x - 32., top + 10., 3., 5., p.seat);
            rect(cr, x - 18., top + 9., 4., 3., 0xece0c4);
        }
    }
}
fn sofa(cr: &Context, x: f64, y: f64, theme: usize, variant: usize) {
    let p = &art::PALETTES[theme];
    let _ = cr.save();
    cr.set_source_rgba(0.20, 0.18, 0.14, 0.18);
    cr.rectangle(x - 42., y - 24., 89., 28.);
    let _ = cr.fill();
    let _ = cr.restore();
    for xx in [x - 37., x + 33.] {
        rect(cr, xx, y - 5., 4., 7., p.wall);
    }
    let separate = matches!(theme, 1 | 4);
    if !separate {
        rect(cr, x - 41., y - 37., 82., 30., p.wall);
        rect(cr, x - 39., y - 39., 78., 28., p.seat);
        rect(cr, x - 39., y - 38., 78., 2., p.accent);
        rect(cr, x - 40., y - 10., 80., 8., p.wood);
    }
    for i in 0..3 {
        let xx = x - 35. + i as f64 * 24.;
        if separate {
            rect(cr, xx - 3., y - 37., 23., 34., p.wood);
            for j in 0..4 {
                rect(cr, xx + j as f64 * 5., y - 34., 2., 18., p.wall);
            }
        } else {
            rect(cr, xx, y - 34., 22., 17., p.seat);
            rect(cr, xx + 20., y - 32., 1., 15., p.rug);
            rect(cr, xx + 10., y - 27., 2., 2., p.rug);
        }
        rect(cr, xx - 1., y - 16., 23., 11., p.seat);
        rect(cr, xx, y - 16., 21., 2., p.accent);
        if (i + variant).is_multiple_of(3) {
            rect(cr, xx + 3., y - 29., 13., 10., p.rug);
            rect(cr, xx + 4., y - 30., 11., 12., p.rug);
            rect(cr, xx + 7., y - 28., 2., 8., p.accent);
        }
    }
    if !separate {
        for xx in [x - 44., x + 38.] {
            rect(cr, xx, y - 24., 6., 21., p.wall);
            rect(cr, xx, y - 25., 6., 17., p.seat);
            rect(cr, xx + 1., y - 25., 4., 2., p.accent);
        }
        // A folded woven throw breaks the long upholstered silhouette.
        if variant.is_multiple_of(2) {
            rect(cr, x + 23., y - 21., 12., 17., p.rug);
            for i in 0..4 {
                rect(cr, x + 24. + i as f64 * 3., y - 21., 1., 19., p.accent);
            }
        }
    }
}
fn draw_actor(cr: &Context, actor: &Actor, frame: u32) {
    let _ = cr.save();
    cr.translate(actor.position.0.round(), actor.position.1.round());
    let action = actor.action();
    let walking = action == Action::Walk;
    let facing = match action {
        Action::Type if !actor.plan.desk_faces_south(actor.slot) => Facing::Up,
        Action::Walk => actor.facing,
        _ => Facing::Down,
    };
    if facing == Facing::Left {
        cr.scale(-1.0, 1.0);
    }
    if matches!(facing, Facing::Left | Facing::Right) {
        cr.scale(0.8, 1.0);
    }
    cr.scale(1.25, 1.25);
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
    // Tails and different body widths keep identities visible even from behind.
    match species {
        0 | 3 | 8 => {
            rect(cr, -14., -13., 7., 8., fur);
            rect(
                cr,
                -16.,
                -17.,
                5.,
                8.,
                if species == 3 { 0xf0ddba } else { fur },
            );
        }
        1 | 2 | 4 | 9 => {
            rect(cr, -11., -14., 22., 9., INK);
            rect(cr, -9., -13., 18., 7., shirt);
        }
        6 | 7 => {
            rect(cr, -11., -15., 22., 8., fur);
        }
        _ => {}
    }
    rect(cr, -8.0, -16.0, 16.0, 13.0, INK);
    rect(cr, -6.0, -16.0, 12.0, 11.0, shirt);
    let outfit = actor.style / 12;
    match outfit % 5 {
        0 => {
            // Work apron.
            rect(cr, -4., -15., 8., 9., 0xe5d4b3);
            rect(cr, -3., -10., 6., 3., shirt);
        }
        1 => {
            // Split jacket and zipper.
            rect(cr, -1., -16., 2., 11., INK);
            rect(cr, -5., -10., 3., 2., 0xf0d5b4);
        }
        2 => {
            // Cross-body satchel, also recognizable from the rear.
            for i in 0..5 {
                rect(
                    cr,
                    -5. + i as f64 * 2.,
                    -16. + i as f64 * 2.,
                    3.,
                    3.,
                    0xe2c495,
                );
            }
            rect(cr, 2., -10., 7., 7., 0x765748);
        }
        3 => {
            // Wide knit stripes.
            for yy in [-13., -8.] {
                rect(cr, -6., yy, 12., 2., 0xf0d9b3);
            }
        }
        _ => {
            // Bib overalls.
            rect(cr, -4., -14., 8., 9., 0x4a6680);
            for xx in [-4., 2.] {
                rect(cr, xx, -16., 2., 5., 0xadc4bc);
            }
        }
    }
    let hands_y = match (action, facing) {
        (Action::Type, Facing::Up) => -22.0,
        (Action::Type, _) => -3.0,
        _ => -13.0,
    };
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
        6 => {
            rect(cr, -4., -33., 3., 5., INK);
            rect(cr, 0., -32., 3., 4., INK);
        }
        7 => {
            for xx in [-10., 5.] {
                rect(cr, xx, -34., 5., 9., fur);
            }
        }
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
        if species == 0 {
            for xx in [-11., 7.] {
                rect(cr, xx, -21., 4., 1., INK);
                rect(cr, xx, -18., 4., 1., INK);
            }
        }
        if species == 9 {
            rect(cr, -3., -24., 6., 7., 0x4c515d);
        }
        if species == 5 {
            rect(cr, -4., -18., 8., 1., 0x456446);
        }
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
    match outfit {
        0 | 5 => {
            // Asymmetric cap leaves the ears visible.
            rect(cr, -7., -31., 14., 3., shirt);
            rect(cr, -5., -35., 10., 5., shirt);
            rect(cr, 4., -31., 7., 2., 0xe4c291);
        }
        1 | 6 => {
            // Headset.
            rect(cr, -9., -30., 18., 2., 0xd9c8a5);
            for xx in [-11., 8.] {
                rect(cr, xx, -26., 3., 7., shirt);
            }
            if facing != Facing::Up {
                rect(cr, 6., -19., 5., 2., INK);
            }
        }
        2 | 7 if facing != Facing::Up => {
            for xx in [-7., 2.] {
                rect(cr, xx, -25., 6., 5., INK);
                rect(cr, xx + 1., -24., 4., 3., 0xc9d8d4);
            }
            rect(cr, -1., -24., 3., 1., INK);
        }
        3 | 8 => {
            rect(cr, -7., -15., 14., 3., 0xe4c291);
            rect(cr, 4., -13., 3., 8., 0xe4c291);
        }
        _ => {
            rect(cr, 4., -30., 6., 4., shirt);
            rect(cr, 6., -33., 3., 8., 0xf0d9b3);
        }
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
            draw_room(&cr, &plan, design, &[&actor], frame, None);
            cr.status().unwrap();
        }
        let bytes = surface.data().unwrap().to_vec();
        bytes
    }
    #[test]
    fn cached_background_preserves_pixels_and_avoids_repainting_shapes() {
        for design in [0, 5, 10, 15, 19, 23] {
            let plan = Plan::new(32, design);
            let scale = 0.75;
            let cache = Background::new(&plan, design, scale).unwrap();
            let render = |cached| {
                let mut surface = gtk::cairo::ImageSurface::create(
                    gtk::cairo::Format::ARgb32,
                    (plan.width * scale).ceil() as i32,
                    (plan.height * scale).ceil() as i32,
                )
                .unwrap();
                {
                    let cr = Context::new(&surface).unwrap();
                    cr.set_antialias(gtk::cairo::Antialias::None);
                    cr.scale(scale, scale);
                    if cached {
                        cache.paint(&cr);
                    } else {
                        art::shell(&cr, &plan, design);
                    }
                }
                let pixels = surface.data().unwrap().to_vec();
                pixels
            };
            assert_eq!(render(false), render(true), "cached design {design}");
            let start = std::time::Instant::now();
            for _ in 0..10 {
                std::hint::black_box(render(false));
            }
            let direct = start.elapsed();
            let start = std::time::Instant::now();
            for _ in 0..10 {
                std::hint::black_box(render(true));
            }
            eprintln!(
                "Office background {design}: direct {direct:?}, cached {:?} (10 frames)",
                start.elapsed()
            );
        }
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
                actor.position = (x, y - 20.0 * OBJECT_SCALE);
                let actors = [&actor];
                draw_room(
                    &cr,
                    &plan,
                    0,
                    if with_actor { &actors } else { &[] },
                    0,
                    None,
                );
            }
            let stride = surface.stride() as usize;
            let offset = (y as usize - (28.0 * OBJECT_SCALE) as usize) * stride + x as usize * 4;
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
    fn reflow_keeps_settled_actors_at_their_places_without_restarting_activity() {
        for status in [
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Idle,
            AgentStatus::Unknown,
        ] {
            let mut actor = Actor::new(0, 0, status, Rc::new(Plan::fit(3, 0, 1.3)));
            for phase in 0..3 {
                actor.rest_phase = phase;
                actor.position = actor.destination();
                actor.rest_elapsed = 2.5;
                actor.status_age = 7.0;
                let action = actor.action();
                for design in 0..24 {
                    for aspect in [0.7, 2.0, 0.7] {
                        actor.replan(0, Rc::new(Plan::fit(3, design, aspect)));
                        assert_eq!(actor.position, actor.destination());
                        assert_eq!(actor.action(), action);
                        assert!(actor.path.is_empty());
                        assert_eq!(actor.rest_elapsed, 2.5);
                        assert_eq!(actor.status_age, 7.0);
                    }
                }
            }
        }
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
        let activity = (
            actor.status_age,
            actor.rest_elapsed,
            actor.rest_phase,
            actor.facing,
        );
        actor.replan(0, next.clone());
        assert_eq!(
            activity,
            (
                actor.status_age,
                actor.rest_elapsed,
                actor.rest_phase,
                actor.facing
            )
        );
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
