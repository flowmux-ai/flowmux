// SPDX-License-Identifier: GPL-3.0-or-later
//! Teammate movement and the office renderer.
//!
//! The static room (floor, walls and every piece of furniture) is baked once
//! per plan and scale. Each frame repaints that surface, then redraws only
//! characters, animated props, and furniture that overlaps a character, in
//! depth order. Sprites contain no partial alpha, so redrawing furniture over
//! its own baked pixels is exact.

use super::{
    character::{self, Dir, Pose},
    layout::{Activity, Item, Kind, Layer, Plan, Rect},
    props::{self, Daylight, Emote},
    sprite,
    theme::{self, Clutter, Feature},
};
use flowmux_core::AgentStatus;
use gtk::cairo::{Context, Filter, Format, ImageSurface};
use std::{collections::VecDeque, rc::Rc};

pub(super) use super::character::SPECIES;
pub(super) use super::theme::{design_name, DESIGNS};

const SPEED: f64 = 76.0;
const ACCELERATION: f64 = 260.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    Visit(Activity),
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
    /// Seconds since walking in through the door, while the arrival sparkles.
    pub arrived: Option<f64>,
    /// Seconds since finishing a task, while confetti falls.
    pub cheer: Option<f64>,
    /// A shared-event spot that overrides the rest cycle while resting.
    pub errand: Option<((f64, f64), Activity)>,
    rest_phase: u8,
    rest_elapsed: f64,
    yielding: f64,
    speed: f64,
    stride: f64,
    clock: f64,
    exit_age: f64,
    /// A courier who is not a teammate: walks to its errand, then leaves.
    visitor: bool,
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
            arrived: None,
            cheer: None,
            errand: None,
            rest_phase: 0,
            rest_elapsed: 0.0,
            yielding: 0.0,
            speed: 0.0,
            stride: 0.0,
            clock: slot as f64 * 1.7,
            exit_age: 0.0,
            visitor: false,
            plan,
            path: VecDeque::new(),
        }
    }

    /// A courier who enters through the door and walks to `target`.
    pub fn visitor(style: usize, plan: Rc<Plan>, target: (f64, f64)) -> Self {
        let mut actor = Self::new(0, style, AgentStatus::Unknown, plan);
        actor.visitor = true;
        actor.errand = Some((target, Activity::Snack));
        actor.arrive();
        actor
    }

    pub fn idle(&self) -> bool {
        self.path.is_empty()
    }

    /// Enters through the door and walks to the current destination.
    pub fn arrive(&mut self) {
        self.position = self.plan.door();
        self.path = self.plan.route(self.position, self.destination());
        self.arrived = Some(0.0);
        self.facing = Facing::Up;
    }

    pub fn replan(&mut self, slot: usize, plan: Rc<Plan>) {
        let settled = self.path.is_empty() && self.position == self.destination();
        self.slot = slot;
        // Moving actors retain their relative position; settled actors stay with their furniture.
        self.position.0 *= plan.width / self.plan.width;
        self.position.1 *= plan.height / self.plan.height;
        self.plan = plan;
        self.errand = None;
        if settled {
            self.position = self.destination();
        } else {
            self.path = self.plan.route(self.position, self.destination());
        }
    }

    fn resting(&self) -> bool {
        self.ended.is_none() && matches!(self.status, AgentStatus::Idle | AgentStatus::Done)
    }

    fn visit(&self) -> Option<Activity> {
        if !self.resting() {
            return None;
        }
        if let Some((_, activity)) = self.errand {
            return Some(activity);
        }
        (self.rest_phase > 0)
            .then(|| self.plan.rest_stop(self.slot, self.rest_phase).1)
            .flatten()
    }

    fn destination(&self) -> (f64, f64) {
        if self.visitor && self.ended.is_none() {
            if let Some((point, _)) = self.errand {
                return point;
            }
        }
        if self.resting() {
            if let Some((point, _)) = self.errand {
                return point;
            }
            if self.rest_phase > 0 {
                return self.plan.rest_stop(self.slot, self.rest_phase).0;
            }
        }
        self.plan
            .destination(self.slot, self.status, self.ended.is_some())
    }

    pub fn set_status(&mut self, status: AgentStatus) {
        if self.status == status && self.ended.is_none() {
            return;
        }
        if status == AgentStatus::Done && self.status != AgentStatus::Done {
            self.cheer = Some(0.0);
        }
        self.status = status;
        self.status_age = 0.0;
        self.rest_phase = 0;
        self.rest_elapsed = 0.0;
        self.yielding = 0.0;
        self.errand = None;
        self.ended = None;
        self.path = self.plan.route(
            self.position,
            self.plan.destination(self.slot, status, false),
        );
    }

    /// Sends a resting teammate to a shared spot, or back to its routine with `None`.
    pub fn set_errand(&mut self, errand: Option<((f64, f64), Activity)>) {
        if !self.resting() || self.errand == errand {
            return;
        }
        self.errand = errand;
        self.rest_elapsed = 0.0;
        self.path = self.plan.route(self.position, self.destination());
    }

    pub fn is_resting(&self) -> bool {
        self.resting()
    }

    pub fn finish(&mut self) {
        if self.ended.is_none() {
            self.ended = Some(0.0);
            self.errand = None;
            self.exit_age = 0.0;
            self.path = self.plan.route(
                self.position,
                self.plan.destination(self.slot, self.status, true),
            );
        }
    }

    fn at_exit(&self) -> bool {
        self.ended.is_some()
            && self.path.is_empty()
            && self.position == self.plan.destination(self.slot, self.status, true)
    }

    pub fn departed(&self) -> bool {
        self.ended.is_some_and(|age| age >= 4.0) && self.at_exit() && self.exit_age >= 0.6
    }

    pub fn action(&self) -> Action {
        if !self.path.is_empty() {
            return Action::Walk;
        }
        if let Some(activity) = self.visit() {
            return Action::Visit(activity);
        }
        match self.status {
            _ if self.ended.is_some() => Action::Wait,
            AgentStatus::Working if self.reading => Action::Read,
            AgentStatus::Working => Action::Type,
            AgentStatus::Done | AgentStatus::Idle => Action::Sit,
            _ => Action::Wait,
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
        let ratio = (dt * SPEED / length).min(1.0);
        let proposed = (
            self.position.0 + delta.0 * ratio,
            self.position.1 + delta.1 * ratio,
        );
        if neighbors.into_iter().any(|other| {
            let distance = (other.0 - proposed.0).hypot(other.1 - proposed.1);
            distance < 12.0
                && distance < (other.0 - self.position.0).hypot(other.1 - self.position.1)
        }) {
            self.yielding += dt;
            self.speed = 0.0;
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

    fn remaining(&self) -> f64 {
        let mut total = 0.0;
        let mut from = self.position;
        for &point in &self.path {
            total += (point.0 - from.0).hypot(point.1 - from.1);
            from = point;
        }
        total
    }

    pub fn advance(&mut self, dt: f64, animate: bool) -> bool {
        self.status_age += dt;
        self.clock += dt;
        if let Some(age) = self.ended.as_mut() {
            *age += dt;
        }
        for timer in [&mut self.arrived, &mut self.cheer] {
            if let Some(age) = timer {
                *age += dt;
                if *age > 2.0 {
                    *timer = None;
                }
            }
        }
        if !animate {
            let changed = !self.path.is_empty();
            self.position = self.destination();
            self.path.clear();
            self.arrived = None;
            self.cheer = None;
            if self.at_exit() {
                self.exit_age = 1.0;
            }
            return changed;
        }
        if self.at_exit() {
            self.exit_age += dt;
        }
        if self.path.is_empty() && self.resting() && self.errand.is_none() {
            self.rest_elapsed += dt;
            let pause = if self.rest_phase == 0 {
                12.0 + (self.slot % 7) as f64
            } else {
                6.0
            };
            if self.rest_elapsed >= pause {
                self.rest_elapsed = 0.0;
                self.rest_phase = (self.rest_phase + 1) % 3;
                self.path = self.plan.route(self.position, self.destination());
            }
        }
        let moving = !self.path.is_empty();
        if !moving {
            self.speed = 0.0;
            return false;
        }
        // Ease in from a standstill and settle gently onto the final spot.
        let remaining = self.remaining();
        let settle = (remaining / 14.0).clamp(0.35, 1.0) * SPEED;
        self.speed = (self.speed + ACCELERATION * dt).min(SPEED).min(settle);
        let mut distance = self.speed.max(SPEED * 0.3) * dt;
        self.stride += distance;
        while let Some(&(x, y)) = self.path.front() {
            let (dx, dy) = (x - self.position.0, y - self.position.1);
            let length = dx.hypot(dy);
            if length > 0.5 {
                // A little hysteresis keeps diagonal walks from flickering between views.
                let horizontal = match self.facing {
                    Facing::Left | Facing::Right => dx.abs() * 1.3 > dy.abs(),
                    _ => dx.abs() > dy.abs() * 1.3,
                };
                self.facing = if horizontal {
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
        if self.path.is_empty() {
            self.speed = 0.0;
        }
        moving
    }

    fn blinking(&self) -> bool {
        let period = 3.4 + (self.slot % 5) as f64 * 0.37;
        self.clock % period < 0.14
    }

    /// The pose to draw; `frame` advances idle and work loops at about six steps a second.
    fn appearance(&self, frame: u32) -> (Dir, bool, Pose) {
        let f = (frame % 4) as u8;
        let toward = |facing: Facing| match facing {
            Facing::Up => (Dir::Back, false),
            Facing::Down => (Dir::Front, false),
            Facing::Right => (Dir::Side, false),
            Facing::Left => (Dir::Side, true),
        };
        let south = self.plan.desk_faces_south(self.slot);
        let desk = if south { Dir::Front } else { Dir::Back };
        match self.action() {
            Action::Walk => {
                let (dir, flip) = toward(self.facing);
                (
                    dir,
                    flip,
                    Pose::Walk(((self.stride / 5.0) as u32 % 4) as u8),
                )
            }
            Action::Type => (desk, false, Pose::Type(f % 2)),
            Action::Read => (desk, false, Pose::Read((frame / 6 % 2) as u8)),
            Action::Sit => (Dir::Front, false, Pose::Sit((frame / 5 % 2) as u8)),
            Action::Wait if self.status == AgentStatus::Blocked => {
                (Dir::Front, false, Pose::Wave((frame / 2 % 2) as u8))
            }
            Action::Wait => (Dir::Front, false, Pose::Stand((frame / 5 % 2) as u8)),
            Action::Visit(activity) => match activity {
                Activity::Coffee | Activity::Snack => {
                    (Dir::Front, false, Pose::Coffee((frame / 3 % 4) as u8))
                }
                Activity::Browse => (Dir::Back, false, Pose::Browse((frame / 4 % 2) as u8)),
                Activity::Tend => (Dir::Side, true, Pose::Tend((frame / 4 % 2) as u8)),
                Activity::Gaze => (Dir::Back, false, Pose::Gaze),
                Activity::Present => (Dir::Back, false, Pose::Present((frame / 4 % 2) as u8)),
            },
        }
    }

    fn emote(&self, frame: u32) -> Option<Emote> {
        if self.ended.is_some() {
            return None;
        }
        if self.visitor {
            return Some(Emote::Pizza);
        }
        let walking = !self.path.is_empty();
        Some(match self.status {
            AgentStatus::Blocked => Emote::Alert,
            AgentStatus::Working if self.reading => Emote::Book,
            AgentStatus::Working => Emote::Typing((frame / 2 % 3) as u8),
            AgentStatus::Done if self.status_age < 8.0 => Emote::Done,
            AgentStatus::Unknown => Emote::Unknown,
            _ if walking => return None,
            _ => match self.visit() {
                Some(Activity::Coffee) => Emote::Coffee,
                Some(Activity::Snack) => Emote::Pizza,
                Some(Activity::Present) => Emote::Heart,
                Some(_) => return None,
                None => Emote::Sleep((frame / 6 % 2) as u8),
            },
        })
    }
}

/// Courier and pizza for a delivery, drawn with the room but never selectable.
#[derive(Default)]
pub(super) struct Extras<'a> {
    pub visitors: Vec<&'a Actor>,
    pub pizza: Option<(f64, f64)>,
}

pub(super) struct Background {
    surface: ImageSurface,
    pub scale: f64,
    pub daylight: Daylight,
}

fn filter(scale: f64) -> Filter {
    if scale >= 1.0 && (scale - scale.round()).abs() < 1e-6 {
        Filter::Nearest
    } else {
        Filter::Good
    }
}

fn cairo_rgba(cr: &Context, color: u32, alpha: f64) {
    cr.set_source_rgba(
        ((color >> 16) & 255) as f64 / 255.0,
        ((color >> 8) & 255) as f64 / 255.0,
        (color & 255) as f64 / 255.0,
        alpha,
    );
}

impl Background {
    /// Bakes the room at `scale` device pixels per art pixel.
    pub fn new(plan: &Plan, scale: f64, daylight: Daylight) -> Result<Self, gtk::cairo::Error> {
        let (w, h) = (plan.width.ceil() as i32, plan.height.ceil() as i32);
        let art = ImageSurface::create(Format::ARgb32, w, h)?;
        {
            let cr = Context::new(&art)?;
            props::shell(&cr, plan.design, w as f64, h as f64);
            let t = theme::theme(plan.design);
            for layer in [Layer::Floor, Layer::Wall] {
                for item in plan.items.iter().filter(|i| i.layer == layer) {
                    let surface = props::item_sprite(item, plan.design, daylight);
                    sprite::paint(&cr, &surface, item.x, item.y);
                }
            }
            // Soft contact shadows ground standing furniture on the floor.
            cairo_rgba(&cr, 0x1d1530, if t.dark { 0.32 } else { 0.16 });
            for item in plan.items.iter().filter(|i| i.layer == Layer::Stand) {
                let r = item.rect();
                if matches!(item.kind, Kind::Door | Kind::Chair { .. }) {
                    continue;
                }
                cr.rectangle(r.x + 2.0, r.y + r.h - 3.0, r.w - 2.0, 3.0);
                cr.rectangle(r.x + 4.0, r.y + r.h, r.w - 6.0, 1.0);
            }
            let _ = cr.fill();
            if t.dark || daylight == Daylight::Night {
                light_pools(&cr, plan, t.dark);
            }
            let mut stands: Vec<_> = plan
                .items
                .iter()
                .filter(|i| i.layer == Layer::Stand && i.kind != Kind::Door)
                .collect();
            stands.sort_by(|a, b| a.sort.total_cmp(&b.sort));
            for item in stands {
                let surface = props::item_sprite(item, plan.design, daylight);
                sprite::paint(&cr, &surface, item.x, item.y);
            }
        }
        let device = ImageSurface::create(
            Format::ARgb32,
            (plan.width * scale).ceil() as i32,
            (plan.height * scale).ceil() as i32,
        )?;
        {
            let cr = Context::new(&device)?;
            cr.scale(scale, scale);
            let _ = cr.set_source_surface(&art, 0.0, 0.0);
            cr.source().set_filter(filter(scale));
            let _ = cr.paint();
        }
        Ok(Self {
            surface: device,
            scale,
            daylight,
        })
    }

    fn paint(&self, cr: &Context) {
        let _ = cr.save();
        cr.scale(1.0 / self.scale, 1.0 / self.scale);
        let _ = cr.set_source_surface(&self.surface, 0.0, 0.0);
        cr.source().set_filter(Filter::Nearest);
        let _ = cr.paint();
        let _ = cr.restore();
    }
}

/// Warm pools under lamps and screens for dark rooms.
fn light_pools(cr: &Context, plan: &Plan, dark: bool) {
    let accent = theme::theme(plan.design).accent;
    for item in &plan.items {
        let (x, y, radius, color) = match item.kind {
            Kind::Clutter(Clutter::FloorLamp) => (item.x + 9.0, item.y + 22.0, 26.0, 0xffd38a),
            Kind::Clutter(Clutter::LavaLamp) => (item.x + 9.0, item.y + 20.0, 20.0, accent),
            Kind::Desk { down: false } if dark => (item.x + 20.0, item.y + 16.0, 30.0, 0x8fd0ff),
            Kind::Feature(Feature::Fireplace) => (item.x + 24.0, item.y + 46.0, 40.0, 0xff9a4a),
            Kind::Feature(Feature::Arcade | Feature::Vending | Feature::ServerRack) => {
                (item.x + 24.0, item.y + 46.0, 30.0, accent)
            }
            _ => continue,
        };
        let gradient = gtk::cairo::RadialGradient::new(x, y, 0.0, x, y, radius);
        let c = |shift: u32| ((color >> shift) & 255) as f64 / 255.0;
        gradient.add_color_stop_rgba(0.0, c(16), c(8), c(0), 0.22);
        gradient.add_color_stop_rgba(1.0, c(16), c(8), c(0), 0.0);
        let _ = cr.set_source(&gradient);
        cr.arc(x, y, radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}

enum Draw<'a> {
    Item(&'a Item),
    Actor(&'a Actor),
    Pizza((f64, f64)),
}

fn actor_box(actor: &Actor) -> Rect {
    Rect {
        x: actor.position.0 - 12.0,
        y: actor.position.1 - 36.0,
        w: 24.0,
        h: 37.0,
    }
}

fn animated(item: &Item, actors: &[&Actor]) -> bool {
    match item.kind {
        Kind::Desk { down: false } => item.slot.is_some_and(|slot| {
            actors
                .iter()
                .any(|a| a.slot == slot && a.status == AgentStatus::Working && a.ended.is_none())
        }),
        Kind::Counter { .. } => true,
        Kind::Feature(f) => matches!(
            f,
            Feature::ServerRack
                | Feature::Fireplace
                | Feature::Arcade
                | Feature::Console
                | Feature::Printer3d
                | Feature::HydroPod
                | Feature::EspressoBar
        ),
        Kind::Clutter(Clutter::LavaLamp) => true,
        _ => false,
    }
}

/// Moving parts painted over an item's sprite in art coordinates.
fn overlay(cr: &Context, item: &Item, design: usize, frame: u32, now: (u32, u32)) {
    let t = theme::theme(design);
    match item.kind {
        Kind::Desk { down: false } => {
            let low = if t.desk == theme::DeskStyle::Low {
                3.0
            } else {
                0.0
            };
            let screen = props::screen(design, frame / 2 + item.slot.unwrap_or(0) as u32);
            sprite::paint(cr, &screen, item.x + 11.0, item.y + 3.0 + low);
        }
        Kind::Counter { .. } => {
            // Steam curls above the espresso machine.
            let phase = (frame / 2 % 4) as f64;
            for (i, dy) in [0.0, 3.0, 6.0].into_iter().enumerate() {
                let dx = if (i as u32 + frame / 2).is_multiple_of(2) {
                    0.0
                } else {
                    1.0
                };
                cairo_rgba(cr, 0xffffff, 0.55 - i as f64 * 0.15);
                cr.rectangle(item.x + 10.0 + dx, item.y - 2.0 - dy - phase, 1.0, 2.0);
                let _ = cr.fill();
            }
        }
        Kind::Clock => {
            let (hour, minute) = now;
            let (cx, cy) = (item.x + 7.0, item.y + 7.0);
            let hand = |angle: f64, length: f64, color: u32| {
                for step in 0..length as i32 {
                    let x = (cx + angle.sin() * step as f64).floor();
                    let y = (cy - angle.cos() * step as f64).floor();
                    props::rect(cr, x, y, 1.0, 1.0, color);
                }
            };
            let tau = std::f64::consts::TAU;
            hand(
                tau * (hour % 12) as f64 / 12.0 + tau * minute as f64 / 720.0,
                3.5,
                props::INK,
            );
            hand(tau * minute as f64 / 60.0, 5.5, 0xd9484f);
        }
        Kind::Feature(f) => {
            let blink = |i: u32| (frame / 2 + i * 3) % 5 < 2;
            match f {
                Feature::ServerRack | Feature::Console | Feature::Printer3d => {
                    for i in 0..6u32 {
                        let color = if blink(i) { t.accent } else { 0x2e3a4f };
                        props::rect(
                            cr,
                            item.x + 10.0 + (i % 3) as f64 * 12.0,
                            item.y + 14.0 + (i / 3) as f64 * 10.0,
                            2.0,
                            1.0,
                            color,
                        );
                    }
                }
                Feature::Fireplace => {
                    for i in 0..5u32 {
                        let h = 3.0 + ((frame + i * 2) % 4) as f64;
                        let color = if i % 2 == 0 { 0xffb347 } else { 0xff7a3d };
                        props::rect(
                            cr,
                            item.x + 16.0 + i as f64 * 3.0,
                            item.y + 36.0 - h,
                            2.0,
                            h,
                            color,
                        );
                    }
                }
                Feature::Arcade | Feature::HydroPod | Feature::EspressoBar => {
                    let color = if (frame / 3).is_multiple_of(2) {
                        t.accent2
                    } else {
                        t.accent
                    };
                    props::rect(cr, item.x + 20.0, item.y + 10.0, 8.0, 2.0, color);
                }
                _ => {}
            }
        }
        Kind::Clutter(Clutter::LavaLamp) => {
            let y = item.y + 8.0 + (frame / 3 % 4) as f64;
            props::rect(cr, item.x + 8.0, y, 2.0, 2.0, sprite::light(t.accent, 0.45));
        }
        _ => {}
    }
}

fn draw_actor(cr: &Context, actor: &Actor, frame: u32, alpha: f64) {
    let (dir, flip, pose) = actor.appearance(frame);
    let surface = character::sprite(actor.style, dir, flip, pose, actor.blinking());
    let (x, y) = (actor.position.0.round(), actor.position.1.round());
    let _ = cr.set_source_surface(&surface, x - 12.0, y - 35.0);
    cr.source().set_filter(Filter::Nearest);
    if alpha < 1.0 {
        let _ = cr.paint_with_alpha(alpha);
    } else {
        let _ = cr.paint();
    }
}

/// Paints one office in art coordinates; `scale` is device pixels per art pixel.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_room(
    cr: &Context,
    plan: &Plan,
    actors: &[&Actor],
    extras: &Extras,
    frame: u32,
    now: (u32, u32),
    scale: f64,
    background: Option<&Background>,
) {
    let built;
    let background = match background {
        Some(background) => background,
        None => {
            built = Background::new(plan, scale, Daylight::at(now.0)).ok();
            match built.as_ref() {
                Some(background) => background,
                None => return,
            }
        }
    };
    background.paint(cr);
    let everyone: Vec<&Actor> = actors
        .iter()
        .copied()
        .chain(extras.visitors.iter().copied())
        .collect();
    let boxes: Vec<_> = everyone.iter().map(|a| actor_box(a)).collect();
    let mut dynamic: Vec<(f64, u8, Draw)> = Vec::new();
    for item in plan.items.iter().filter(|i| i.layer == Layer::Stand) {
        if item.kind == Kind::Door {
            continue;
        }
        let rect = item.rect();
        if animated(item, actors) || boxes.iter().any(|b| b.intersects(&rect)) {
            dynamic.push((item.sort, 0, Draw::Item(item)));
        }
    }
    if let Some(table) = extras.pizza {
        dynamic.push((table.1 + 0.5, 1, Draw::Pizza(table)));
    }
    for actor in &everyone {
        dynamic.push((actor.position.1, 2, Draw::Actor(actor)));
    }
    dynamic.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, _, draw) in &dynamic {
        match draw {
            Draw::Item(item) => {
                let surface = props::item_sprite(item, plan.design, background.daylight);
                sprite::paint(cr, &surface, item.x, item.y);
                if animated(item, actors) {
                    overlay(cr, item, plan.design, frame, now);
                }
            }
            Draw::Pizza((x, y)) => sprite::paint(cr, &props::pizza_box(), x - 9.0, y - 7.0),
            Draw::Actor(actor) => {
                let alpha = if actor.at_exit() {
                    (1.0 - actor.exit_age / 0.6).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                draw_actor(cr, actor, frame, alpha);
            }
        }
    }
    // Wall-mounted pieces never overlap characters, but the clock still ticks.
    for item in plan.items.iter().filter(|i| i.kind == Kind::Clock) {
        overlay(cr, item, plan.design, frame, now);
    }
    let door = plan.door();
    let open = everyone
        .iter()
        .any(|a| (a.position.0 - door.0).abs() < 22.0 && (a.position.1 - door.1).abs() < 34.0);
    props::front_wall(cr, plan.design, plan.width, plan.height, door.0, open);
    effects(cr, &everyone, frame, scale);
}

fn effects(cr: &Context, actors: &[&Actor], frame: u32, scale: f64) {
    // Emotes keep at least one device pixel per art pixel in crowded overviews.
    let zoom = (1.0 / scale).max(1.0);
    for actor in actors {
        let (x, y) = actor.position;
        if let Some(age) = actor.arrived {
            for i in 0..4u8 {
                let phase = ((age * 6.0) as u8).wrapping_add(i) % 4;
                let angle = i as f64 * 1.7 + age * 3.0;
                sprite::paint(
                    cr,
                    &props::sparkle(phase),
                    (x + angle.cos() * 13.0 - 3.0).round(),
                    (y - 18.0 + angle.sin() * 16.0 - 3.0).round(),
                );
            }
        }
        if actor.at_exit() && actor.exit_age < 0.6 {
            let phase = (actor.exit_age * 8.0) as u8 % 4;
            sprite::paint(cr, &props::sparkle(phase), x - 3.0, y - 24.0);
        }
        if let Some(age) = actor.cheer {
            for i in 0..14u32 {
                let spread = (i as f64 * 2.39).sin() * 18.0;
                let lift = 26.0 + (i % 4) as f64 * 4.0;
                let t = age * 1.6;
                let px = x + spread * t.min(1.0);
                let py = y - 40.0 - lift * (1.0 - t).max(0.0) + t * t * 14.0;
                props::rect(
                    cr,
                    px.round(),
                    py.round(),
                    if i % 3 == 0 { 2.0 } else { 1.0 },
                    1.0,
                    props::confetti_color(i as usize),
                );
            }
        }
        let Some(kind) = actor.emote(frame) else {
            continue;
        };
        let seated = matches!(actor.action(), Action::Type | Action::Read | Action::Sit);
        let bob = match kind {
            Emote::Alert if frame % 4 < 2 => 1.0,
            _ => 0.0,
        };
        let surface = props::emote(kind);
        let _ = cr.save();
        let top = y - 36.0 + if seated { 3.0 } else { 0.0 } - 15.0 * zoom - bob;
        cr.translate((x + 4.0).round(), top.round());
        cr.scale(zoom, zoom);
        let _ = cr.set_source_surface(&surface, 0.0, 0.0);
        cr.source().set_filter(filter(scale * zoom));
        let _ = cr.paint();
        let _ = cr.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn render(plan: &Rc<Plan>, actors: &[&Actor], frame: u32) -> Vec<u8> {
        let mut surface = ImageSurface::create(
            Format::ARgb32,
            plan.width.ceil() as i32,
            plan.height.ceil() as i32,
        )
        .unwrap();
        {
            let cr = Context::new(&surface).unwrap();
            draw_room(
                &cr,
                plan,
                actors,
                &Extras::default(),
                frame,
                (10, 10),
                1.0,
                None,
            );
        }
        let data = surface.data().unwrap().to_vec();
        data
    }

    #[test]
    fn cached_background_matches_a_fresh_bake() {
        for design in [0, 13, 27, 46] {
            let plan = Rc::new(Plan::fit(4, design, 1.6));
            let actors: Vec<_> = (0..4)
                .map(|slot| Actor::new(slot, slot * 11, AgentStatus::Working, plan.clone()))
                .collect();
            let refs: Vec<_> = actors.iter().collect();
            let fresh = render(&plan, &refs, 2);
            let background = Background::new(&plan, 1.0, Daylight::Day).unwrap();
            let mut surface = ImageSurface::create(
                Format::ARgb32,
                plan.width.ceil() as i32,
                plan.height.ceil() as i32,
            )
            .unwrap();
            {
                let cr = Context::new(&surface).unwrap();
                draw_room(
                    &cr,
                    &plan,
                    &refs,
                    &Extras::default(),
                    2,
                    (10, 10),
                    1.0,
                    Some(&background),
                );
            }
            assert_eq!(fresh, surface.data().unwrap().to_vec(), "design={design}");
        }
    }

    #[test]
    fn desk_occludes_a_character_walking_behind_it() {
        let plan = Rc::new(Plan::fit(2, 0, 1.6));
        let desk = *plan
            .items
            .iter()
            .find(|i| matches!(i.kind, Kind::Desk { down: false }))
            .unwrap();
        let sample = |with_actor: bool| {
            let mut actor = Actor::new(0, 0, AgentStatus::Idle, plan.clone());
            // Feet just behind the desk's top edge put the body behind its front.
            actor.position = (desk.x + 20.0, desk.y + 14.0);
            actor.path.push_back((desk.x + 30.0, desk.y + 14.0));
            let actors = [&actor];
            let pixels = render(&plan, if with_actor { &actors } else { &[] }, 0);
            let stride = (plan.width.ceil() as usize) * 4;
            let (x, y) = ((desk.x + 20.0) as usize, (desk.y + 22.0) as usize);
            pixels[y * stride + x * 4..y * stride + x * 4 + 4].to_vec()
        };
        assert_eq!(sample(true), sample(false));
    }

    #[test]
    fn rest_cycle_visits_points_of_interest_and_returns_to_work() {
        let plan = Rc::new(Plan::fit(3, 0, 1.6));
        let mut actor = Actor::new(0, 0, AgentStatus::Done, plan.clone());
        let mut visits = HashSet::new();
        for _ in 0..1200 {
            actor.advance(0.1, true);
            if let Action::Visit(activity) = actor.action() {
                visits.insert(activity);
            }
        }
        assert!(!visits.is_empty(), "resting teammates leave the lounge");
        actor.set_status(AgentStatus::Working);
        for _ in 0..400 {
            actor.advance(0.1, true);
        }
        assert_eq!(actor.position, plan.desk(0));
        assert_eq!(actor.action(), Action::Type);
        let mut still = Actor::new(1, 0, AgentStatus::Idle, plan.clone());
        for _ in 0..400 {
            still.advance(0.1, false);
        }
        assert_eq!(
            still.rest_phase, 0,
            "reduced motion disables ambient wandering"
        );
        assert_eq!(still.position, plan.seat(1));
    }

    #[test]
    fn crowd_yield_is_bounded() {
        let plan = Rc::new(Plan::fit(3, 0, 1.6));
        let mut actor = Actor::new(0, 0, AgentStatus::Working, plan.clone());
        actor.set_status(AgentStatus::Done);
        let blocker = actor.path.front().copied().unwrap();
        let mut yields = 0;
        for _ in 0..40 {
            if actor.yield_to(0.05, std::iter::once(blocker)) {
                yields += 1;
            } else {
                actor.advance(0.05, true);
            }
        }
        assert!(yields > 0 && yields < 40);
    }

    #[test]
    fn walking_eases_and_turns_through_four_views() {
        let plan = Rc::new(Plan::fit(2, 4, 1.6));
        let mut actor = Actor::new(0, 3, AgentStatus::Working, plan.clone());
        actor.set_status(AgentStatus::Idle);
        let start = actor.position;
        actor.advance(0.05, true);
        let first = (actor.position.0 - start.0).hypot(actor.position.1 - start.1);
        let mut previous = actor.position;
        let mut fastest: f64 = 0.0;
        for _ in 0..200 {
            actor.advance(0.05, true);
            fastest =
                fastest.max((actor.position.0 - previous.0).hypot(actor.position.1 - previous.1));
            previous = actor.position;
        }
        assert!(first < fastest, "walks start slower than cruising speed");
        assert!(fastest <= SPEED * 0.05 + 1e-6);
        let mut views = HashSet::new();
        for facing in [Facing::Up, Facing::Down, Facing::Left, Facing::Right] {
            actor.facing = facing;
            actor.path.push_back((0.0, 0.0));
            views.insert(actor.appearance(0));
            actor.path.clear();
        }
        assert_eq!(views.len(), 4);
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
            let plan = Rc::new(Plan::fit(4, 1, 1.6));
            let mut actor = Actor::new(2, 5, status, plan.clone());
            let age = 3.0;
            actor.status_age = age;
            let next = Rc::new(Plan::fit(4, 1, 0.8));
            actor.replan(2, next.clone());
            assert!(actor.path.is_empty(), "{status:?}");
            assert_eq!(actor.position, next.destination(2, status, false));
            assert_eq!(actor.status_age, age);
        }
    }

    #[test]
    fn arrivals_enter_through_the_door_and_departures_fade_out() {
        let plan = Rc::new(Plan::fit(3, 9, 1.6));
        let mut actor = Actor::new(1, 2, AgentStatus::Working, plan.clone());
        actor.arrive();
        assert_eq!(actor.position, plan.door());
        for _ in 0..600 {
            actor.advance(0.05, true);
        }
        assert_eq!(actor.position, plan.desk(1));
        actor.finish();
        for _ in 0..200 {
            actor.advance(0.05, true);
        }
        assert!(actor.departed());
    }

    #[test]
    fn every_state_has_a_pose_and_emote_and_reduced_motion_holds_still() {
        for design in 0..DESIGNS {
            let plan = Rc::new(Plan::fit(8, design, 1.6));
            let mut actor = Actor::new(4, 17, AgentStatus::Working, plan.clone());
            for status in [
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Idle,
                AgentStatus::Unknown,
                AgentStatus::Working,
            ] {
                actor.set_status(status);
                for _ in 0..400 {
                    actor.advance(0.1, true);
                    if actor.path.is_empty() {
                        break;
                    }
                }
                assert!(actor.path.is_empty(), "design={design} {status:?}");
                assert_eq!(actor.position, plan.destination(4, status, false));
                assert!(actor.emote(0).is_some(), "{status:?}");
            }
        }
        let plan = Rc::new(Plan::fit(2, 0, 1.6));
        let mut actor = Actor::new(0, 0, AgentStatus::Working, plan.clone());
        actor.set_status(AgentStatus::Done);
        assert!(actor.advance(0.1, false));
        assert_eq!(actor.position, plan.seat(0));
        assert!(!actor.advance(0.1, false));
    }

    #[test]
    fn hundred_twenty_characters_and_forty_eight_distinct_designs() {
        let plan = Rc::new(Plan::fit(1, 0, 1.6));
        let looks: HashSet<_> = (0..120)
            .map(|style| {
                let actor = Actor::new(0, style, AgentStatus::Working, plan.clone());
                render(&plan, &[&actor], 0)
            })
            .collect();
        assert_eq!(looks.len(), 120);
        let rooms: HashSet<_> = (0..DESIGNS)
            .map(|design| render(&Rc::new(Plan::fit(3, design, 1.6)), &[], 0))
            .collect();
        assert_eq!(rooms.len(), DESIGNS);
        assert_eq!(
            (0..DESIGNS).map(design_name).collect::<HashSet<_>>().len(),
            DESIGNS
        );
    }
}
