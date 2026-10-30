// SPDX-License-Identifier: GPL-3.0-or-later
//! Mapped-window churn: real drawing, status changes, resizing and ownership release.
use super::*;
use std::time::{Duration, Instant};

pub(crate) async fn check() {
    let (bridge, _receiver) = Bridge::new();
    let workspaces: Vec<_> = (0..4)
        .map(|i| (WorkspaceId::new(), format!("Stress office {i}")))
        .collect();
    let mut model = AgentBarModel {
        visible: true,
        items: (0..32)
            .map(|i| AgentBarItem {
                status_evidence: None,
                pending_waits: Vec::new(),
                turn_outcome: None,
                workspace: workspaces[i / 8].0,
                pane: PaneId::new(),
                surface: SurfaceId::new(),
                surface_label: "Stress terminal".into(),
                agent_name: "codex".into(),
                status: AgentStatus::Idle,
                visual_status: flowmux_core::AgentBarVisualStatus::Waiting,
                seen: true,
                status_text: "idle".into(),
                color: "#ffffff".into(),
            })
            .collect(),
    };
    let window = gtk::Window::builder()
        .default_width(1000)
        .default_height(700)
        .build();
    let mut worst = Duration::ZERO;
    for cycle in 0..12 {
        let office = AgentOffice::new(bridge.clone());
        office.render(&workspaces, &model);
        let weak_world = Rc::downgrade(&office.world);
        let weak_drawing = office.drawing.downgrade();
        let weak_stage = office.stage.downgrade();
        window.set_child(Some(&office.root));
        window.present();
        glib_wait(|| office.drawing.is_mapped() && office.drawing.width() > 0).await;
        for step in 0..8 {
            let start = Instant::now();
            for (index, item) in model.items.iter_mut().enumerate() {
                item.status = [
                    AgentStatus::Working,
                    AgentStatus::Blocked,
                    AgentStatus::Done,
                    AgentStatus::Idle,
                ][(index + step) % 4];
            }
            office.render(&workspaces, &model);
            // Exercise selection and the shared event while the real tick callback runs.
            {
                let mut world = office.world.borrow_mut();
                world.selected = (step % 2 == 0).then_some(workspaces[step % 4].0);
                for room in &mut world.rooms {
                    room.event_wait = 0.0;
                }
                world.place(
                    &office.stage,
                    office.drawing.width(),
                    office.drawing.height(),
                );
            }
            resize(&window, 900 + step as i32 * 35, 650 + step as i32 * 10);
            office.drawing.queue_draw();
            gtk::glib::timeout_future(Duration::from_millis(80)).await;
            worst = worst.max(start.elapsed());
            assert!(
                office
                    .world
                    .borrow()
                    .rooms
                    .iter()
                    .any(|room| room.background.borrow().is_some()),
                "mapped office never painted a room"
            );
        }
        office.render(
            &workspaces,
            &AgentBarModel {
                visible: false,
                items: Vec::new(),
            },
        );
        assert!(office.world.borrow().residents.is_empty());
        assert!(office.world.borrow().rooms.is_empty());
        office.render(&workspaces, &model);
        assert_eq!(office.world.borrow().residents.len(), 32);
        window.set_child(gtk::Widget::NONE);
        drop(office);
        glib_wait(|| {
            weak_world.upgrade().is_none()
                && weak_drawing.upgrade().is_none()
                && weak_stage.upgrade().is_none()
        })
        .await;
        assert_eq!(
            sprite::cache_usage(),
            (0, 0),
            "closed office retained sprites"
        );
        #[cfg(target_os = "macos")]
        {
            let rss = std::process::Command::new("ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .unwrap();
            println!(
                "OFFICE_CHURN cycle={cycle} rss_kib={}",
                String::from_utf8_lossy(&rss.stdout).trim()
            );
        }
        #[cfg(not(target_os = "macos"))]
        println!("OFFICE_CHURN cycle={cycle}");
    }
    window.destroy();
    assert!(
        worst < Duration::from_secs(5),
        "office main thread stalled: {worst:?}"
    );
    println!(
        "OFFICE_STABILITY_OK cycles=12 residents=32 worst_step_ms={}",
        worst.as_millis()
    );
}

async fn glib_wait(mut ready: impl FnMut() -> bool) {
    gtk::glib::future_with_timeout(Duration::from_secs(10), async {
        while !ready() {
            gtk::glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("office mapping or release timed out");
}

fn resize(window: &gtk::Window, width: i32, height: i32) {
    #[cfg(target_os = "macos")]
    {
        use gtk::glib::translate::ToGlibPtr;
        unsafe extern "C" {
            fn gdk_macos_surface_get_native_window(
                surface: *mut gtk::gdk::ffi::GdkSurface,
            ) -> *mut std::ffi::c_void;
        }
        let surface = window.surface().unwrap();
        let native = unsafe {
            &*gdk_macos_surface_get_native_window(surface.to_glib_none().0)
                .cast::<objc2_app_kit::NSWindow>()
        };
        native.setContentSize(objc2_foundation::NSSize::new(width.into(), height.into()));
    }
    #[cfg(not(target_os = "macos"))]
    window.set_default_size(width, height);
}

#[cfg(not(target_os = "macos"))]
#[gtk::test]
async fn mapped_office_churn_releases_its_world_and_caches() {
    check().await;
}
