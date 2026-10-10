// SPDX-License-Identifier: GPL-3.0-or-later
//! Offscreen renders for reviewing office art and timing the draw path.
//!
//! `FLOWMUX_OFFICE_PREVIEW_DIR=/tmp/office cargo test -p flowmux --bins
//! agent_office::preview -- --ignored --nocapture`

use super::{
    layout::{self, Plan, Rect},
    props::Daylight,
    scene::{self, Actor, Extras},
};
use flowmux_core::AgentStatus;
use gtk::cairo::{Context, Format, ImageSurface};
use std::{path::PathBuf, rc::Rc, time::Instant};

const STATUSES: [AgentStatus; 5] = [
    AgentStatus::Working,
    AgentStatus::Working,
    AgentStatus::Blocked,
    AgentStatus::Done,
    AgentStatus::Idle,
];

fn actors(plan: &Rc<Plan>, count: usize) -> Vec<Actor> {
    (0..count)
        .map(|slot| Actor::new(slot, slot * 37 % 120, STATUSES[slot % 5], plan.clone()))
        .collect()
}

fn paint_room(cr: &Context, bounds: Rect, count: usize, design: usize, frame: u32, hour: u32) {
    let plan = Rc::new(Plan::fit(count, design, layout::ASPECT));
    let scale = (bounds.w / plan.width).min(bounds.h / plan.height);
    let background = scene::Background::new(&plan, scale, Daylight::at(hour)).ok();
    let mut actors = actors(&plan, count);
    // Let walkers settle and resting teammates start their routines.
    for actor in &mut actors {
        for _ in 0..(frame as usize * 3) {
            actor.advance(0.1, true);
        }
    }
    let refs: Vec<_> = actors.iter().collect();
    let _ = cr.save();
    cr.rectangle(bounds.x, bounds.y, bounds.w, bounds.h);
    cr.clip();
    cr.translate(bounds.x, bounds.y);
    cr.scale(scale, scale);
    scene::draw_room(
        cr,
        &plan,
        &refs,
        &Extras::default(),
        frame,
        (hour, 25),
        scale,
        background.as_ref(),
    );
    let _ = cr.restore();
}

fn save(mut surface: ImageSurface, dir: &std::path::Path, name: &str) {
    surface.flush();
    let (w, h, stride) = (
        surface.width() as usize,
        surface.height() as usize,
        surface.stride() as usize,
    );
    let data = surface.data().unwrap();
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let p = u32::from_ne_bytes(
                data[y * stride + x * 4..y * stride + x * 4 + 4]
                    .try_into()
                    .unwrap(),
            );
            let a = p >> 24;
            let un = |c: u32| (c * 255).checked_div(a).map_or(0, |v| v.min(255) as u8);
            rgba.extend([
                un((p >> 16) & 255),
                un((p >> 8) & 255),
                un(p & 255),
                a as u8,
            ]);
        }
    }
    image::RgbaImage::from_raw(w as u32, h as u32, rgba)
        .unwrap()
        .save(dir.join(format!("{name}.png")))
        .unwrap();
}

fn output_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("FLOWMUX_OFFICE_PREVIEW_DIR")?);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

#[test]
#[ignore = "writes review images; set FLOWMUX_OFFICE_PREVIEW_DIR"]
fn render_office_previews() {
    let Some(dir) = output_dir() else {
        return;
    };
    let designs = scene::DESIGNS;
    // Every design with a mixed team, as a contact sheet.
    let (cell_w, cell_h, columns) = (420.0, 280.0, 4);
    let rows = designs.div_ceil(columns);
    let sheet = ImageSurface::create(
        Format::ARgb32,
        (cell_w * columns as f64) as i32,
        (cell_h * rows as f64) as i32,
    )
    .unwrap();
    {
        let cr = Context::new(&sheet).unwrap();
        for design in 0..designs {
            let bounds = Rect {
                x: (design % columns) as f64 * cell_w,
                y: (design / columns) as f64 * cell_h,
                w: cell_w - 4.0,
                h: cell_h - 4.0,
            };
            paint_room(&cr, bounds, 5, design, 3, 11);
        }
    }
    save(sheet, &dir, "designs");
    // Single, medium and dense offices at a full-window size.
    for (name, count, design, w, h) in [
        ("solo", 1, 0, 960.0, 560.0),
        ("team", 6, 4, 1280.0, 620.0),
        ("dense", 32, 8, 1280.0, 620.0),
        ("tall", 3, 12, 600.0, 900.0),
    ] {
        let surface = ImageSurface::create(Format::ARgb32, w as i32, h as i32).unwrap();
        {
            let cr = Context::new(&surface).unwrap();
            paint_room(
                &cr,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w,
                    h,
                },
                count,
                design % designs,
                5,
                if name == "tall" { 21 } else { 11 },
            );
        }
        save(surface, &dir, name);
    }
    // Overview: eight offices sharing one window.
    let surface = ImageSurface::create(Format::ARgb32, 1280, 620).unwrap();
    {
        let cr = Context::new(&surface).unwrap();
        let counts = [3, 1, 1, 2, 1, 4, 1, 2];
        let sizes: Vec<_> = counts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let plan = Plan::fit(*c, (i * 7) % designs, layout::ASPECT);
                (plan.width, plan.height)
            })
            .collect();
        let (_, bounds) = layout::arrange(
            &sizes,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1280.0,
                h: 620.0,
            },
        );
        for (i, rect) in bounds.into_iter().enumerate() {
            paint_room(&cr, rect, counts[i], (i * 7) % designs, 2, 15);
        }
    }
    save(surface, &dir, "overview");
}

/// Every species and outfit in each view, plus the pose set, enlarged for review.
#[test]
#[ignore = "writes review images; set FLOWMUX_OFFICE_PREVIEW_DIR"]
fn render_character_sheet() {
    use super::character::{sprite, Dir, Pose, HEIGHT, WIDTH};
    let Some(dir) = output_dir() else {
        return;
    };
    let zoom = 4.0;
    let poses = [
        (Dir::Front, Pose::Stand(0)),
        (Dir::Back, Pose::Stand(0)),
        (Dir::Side, Pose::Stand(0)),
        (Dir::Side, Pose::Walk(0)),
        (Dir::Side, Pose::Walk(1)),
        (Dir::Side, Pose::Walk(2)),
        (Dir::Front, Pose::Walk(0)),
        (Dir::Front, Pose::Walk(2)),
        (Dir::Back, Pose::Walk(0)),
        (Dir::Front, Pose::Type(0)),
        (Dir::Back, Pose::Type(1)),
        (Dir::Front, Pose::Read(0)),
        (Dir::Front, Pose::Sit(0)),
        (Dir::Front, Pose::Coffee(2)),
        (Dir::Front, Pose::Wave(0)),
        (Dir::Back, Pose::Browse(1)),
        (Dir::Back, Pose::Gaze),
        (Dir::Side, Pose::Tend(1)),
    ];
    let columns = poses.len().max(12);
    let rows = 12 + 2;
    let (cw, ch) = (WIDTH as f64 + 4.0, HEIGHT as f64 + 4.0);
    let surface = ImageSurface::create(
        Format::ARgb32,
        (cw * columns as f64 * zoom) as i32,
        (ch * rows as f64 * zoom) as i32,
    )
    .unwrap();
    {
        let cr = Context::new(&surface).unwrap();
        cr.set_source_rgb(0.86, 0.82, 0.74);
        let _ = cr.paint();
        cr.scale(zoom, zoom);
        // Rows 0-11: each species, cycling outfits, in three views and three walk frames.
        for species in 0..12 {
            for (col, (dir, pose)) in poses.iter().enumerate().take(columns) {
                let style = species + 12 * ((species + col) % 10);
                let s = sprite(style, *dir, false, *pose, false);
                super::sprite::paint(&cr, &s, col as f64 * cw + 2.0, species as f64 * ch + 2.0);
            }
        }
        // Rows 12-13: one species in all ten outfits, front and back.
        for outfit in 0..10 {
            for (row, dir) in [Dir::Front, Dir::Back].into_iter().enumerate() {
                let s = sprite(outfit * 12 + 2, dir, false, Pose::Stand(0), false);
                super::sprite::paint(
                    &cr,
                    &s,
                    outfit as f64 * cw + 2.0,
                    (12 + row) as f64 * ch + 2.0,
                );
            }
        }
    }
    save(surface, &dir, "characters");
}

#[test]
#[ignore = "timing report; run with --release"]
fn bench_office_frames() {
    let counts = [6, 1, 3, 2, 1, 4, 1, 32];
    let sizes: Vec<_> = counts
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let plan = Plan::fit(*c, (i * 5) % scene::DESIGNS, layout::ASPECT);
            (plan.width, plan.height)
        })
        .collect();
    let (_, bounds) = layout::arrange(
        &sizes,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1280.0,
            h: 620.0,
        },
    );
    let start = Instant::now();
    let rooms: Vec<_> = bounds
        .iter()
        .enumerate()
        .map(|(i, rect)| {
            let design = (i * 5) % scene::DESIGNS;
            let plan = Rc::new(Plan::fit(counts[i], design, layout::ASPECT));
            let scale = (rect.w / plan.width).min(rect.h / plan.height);
            let actors = actors(&plan, counts[i]);
            (*rect, design, plan, scale, actors)
        })
        .collect();
    let planning = start.elapsed();
    let surface = ImageSurface::create(Format::ARgb32, 1280, 620).unwrap();
    let bake = || {
        let start = Instant::now();
        let backgrounds: Vec<_> = rooms
            .iter()
            .map(|(_, _, plan, scale, _)| scene::Background::new(plan, *scale, Daylight::Day).ok())
            .collect();
        (backgrounds, start.elapsed())
    };
    let (_, cold) = bake();
    let (backgrounds, build) = bake();
    let frames = 120;
    let start = Instant::now();
    for frame in 0..frames {
        let cr = Context::new(&surface).unwrap();
        cr.set_source_rgb(0.07, 0.10, 0.14);
        let _ = cr.paint();
        for ((rect, _, plan, scale, actors), background) in rooms.iter().zip(&backgrounds) {
            let refs: Vec<_> = actors.iter().collect();
            let _ = cr.save();
            cr.rectangle(rect.x, rect.y, rect.w, rect.h);
            cr.clip();
            cr.translate(rect.x, rect.y);
            cr.scale(*scale, *scale);
            scene::draw_room(
                &cr,
                plan,
                &refs,
                &Extras::default(),
                frame,
                (11, 0),
                *scale,
                background.as_ref(),
            );
            let _ = cr.restore();
        }
        surface.flush();
    }
    let per_frame = start.elapsed() / frames;
    eprintln!(
        "office bench: plans {planning:?}, backgrounds {build:?} (cold {cold:?}), frame {per_frame:?} ({} agents)",
        counts.iter().sum::<usize>()
    );
}

/// Every prop kind under all twelve themes, enlarged: one sheet per prop family.
#[test]
#[ignore = "writes review images; set FLOWMUX_OFFICE_PREVIEW_DIR"]
fn render_prop_sheets() {
    use super::{
        layout::Kind,
        props::{self, Pal},
        theme::{Clutter, Decor, Feature, Plant, THEMES},
    };
    let Some(dir) = output_dir() else {
        return;
    };
    let features = [
        Feature::Easel,
        Feature::Bookcase,
        Feature::Planter,
        Feature::PottingBench,
        Feature::ServerRack,
        Feature::Telescope,
        Feature::Globe,
        Feature::Workbench,
        Feature::Printer3d,
        Feature::EspressoBar,
        Feature::PastryCase,
        Feature::Arcade,
        Feature::Vending,
        Feature::HydroPod,
        Feature::Console,
        Feature::Fireplace,
        Feature::WoodPile,
        Feature::SurfRack,
        Feature::DeckChair,
        Feature::Bonsai,
        Feature::StoneLantern,
        Feature::Gumball,
    ];
    let decor = [
        Decor::Pinboard,
        Decor::Frame,
        Decor::HangingPlants,
        Decor::Shelf,
        Decor::MonitorWall,
        Decor::StarChart,
        Decor::Pegboard,
        Decor::Poster,
        Decor::Chalkboard,
        Decor::Bunting,
        Decor::Neon,
        Decor::Porthole,
        Decor::Skis,
        Decor::Lifebuoy,
        Decor::Scroll,
    ];
    let mut small: Vec<Kind> = [
        Plant::Monstera,
        Plant::Fern,
        Plant::Palm,
        Plant::Snake,
        Plant::Cactus,
        Plant::Fir,
        Plant::Bamboo,
    ]
    .into_iter()
    .map(Kind::Plant)
    .collect();
    small.extend(
        [
            Clutter::FloorLamp,
            Clutter::Crates,
            Clutter::WateringCan,
            Clutter::SmallPot,
            Clutter::Bin,
            Clutter::BookPile,
            Clutter::Stool,
            Clutter::LavaLamp,
            Clutter::BeanBag,
            Clutter::Cushion,
            Clutter::PaperLantern,
        ]
        .into_iter()
        .map(Kind::Clutter),
    );
    let shared = vec![
        Kind::Desk { down: false },
        Kind::Desk { down: true },
        Kind::Chair { down: false },
        Kind::Chair { down: true },
        Kind::Sofa,
        Kind::Armchair,
        Kind::CoffeeTable,
        Kind::BeanBag,
        Kind::SideTable,
        Kind::Counter { w: 70 },
        Kind::Fridge,
        Kind::WaterCooler,
        Kind::Printer,
        Kind::Whiteboard,
        Kind::Clock,
        Kind::Window { w: 56 },
    ];
    let sheets: [(&str, Vec<Kind>); 4] = [
        (
            "props-features",
            features.into_iter().map(Kind::Feature).collect(),
        ),
        ("props-decor", decor.into_iter().map(Kind::Decor).collect()),
        ("props-small", small),
        ("props-shared", shared),
    ];
    for (name, kinds) in sheets {
        let zoom = 3.0;
        let (cw, ch) = (
            kinds.iter().map(|k| k.size().0).max().unwrap() as f64 + 6.0,
            kinds.iter().map(|k| k.size().1).max().unwrap() as f64 + 6.0,
        );
        // Columns are kinds; rows are the twelve themes, on each theme's own floor color.
        let surface = ImageSurface::create(
            Format::ARgb32,
            (cw * kinds.len() as f64 * zoom) as i32,
            (ch * THEMES.len() as f64 * zoom) as i32,
        )
        .unwrap();
        {
            let cr = Context::new(&surface).unwrap();
            cr.scale(zoom, zoom);
            for (row, theme) in THEMES.iter().enumerate() {
                let floor = theme.floor_colors.0;
                props::rect(
                    &cr,
                    0.0,
                    row as f64 * ch,
                    cw * kinds.len() as f64,
                    ch,
                    floor,
                );
                let pal = Pal::new(row * super::theme::LAYOUTS);
                for (col, kind) in kinds.iter().enumerate() {
                    let canvas = props::build(*kind, &pal, row % 3, Daylight::Day);
                    let (w, h) = kind.size();
                    super::sprite::paint(
                        &cr,
                        &canvas.bake(),
                        col as f64 * cw + (cw - w as f64) / 2.0,
                        row as f64 * ch + (ch - h as f64) / 2.0,
                    );
                }
            }
        }
        save(surface, &dir, name);
    }
}
