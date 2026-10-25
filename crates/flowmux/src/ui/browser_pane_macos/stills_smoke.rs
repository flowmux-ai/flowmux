// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

pub(crate) async fn check(window: &gtk::Window, suspended: bool, expect_stills: bool) {
    let views = NATIVE_VIEW_STILLS.with_borrow(|stills| {
        stills
            .iter()
            .filter_map(|still| {
                let placeholder = still.placeholder.upgrade()?;
                (placeholder.is_mapped()
                    && placeholder.root().as_ref() == Some(window.upcast_ref::<gtk::Root>()))
                .then(|| (still.web_view.load().unwrap(), still.image.clone()))
            })
            .collect::<Vec<_>>()
    });
    assert!(!views.is_empty(), "dialog fixture needs a visible web pane");
    glib::future_with_timeout(Duration::from_secs(30), async {
        while views.iter().any(|(view, image)| {
            view.as_super().isHidden() != suspended || image.borrow().is_some() != expect_stills
        }) {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "dialog stills suspended={suspended}: {:?}",
            views
                .iter()
                .map(|(view, image)| (
                    view.as_super().isHidden(),
                    image.borrow().is_some(),
                    native_view_still_pending(view.as_super())
                ))
                .collect::<Vec<_>>()
        )
    });
    if expect_stills {
        for (_, image) in &views {
            image
                .borrow()
                .as_ref()
                .unwrap()
                .with_data(|pixels| {
                    assert!(
                        pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0),
                        "pane still must contain rendered pixels"
                    );
                })
                .unwrap();
        }
    }
}
