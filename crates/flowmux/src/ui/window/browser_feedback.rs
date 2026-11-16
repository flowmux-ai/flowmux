// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::ui::browser_feedback::{Action, Capture, Selection, PICK, POLL, STOP};
use crate::ui::browser_pane::BrowserPane;

impl WindowController {
    pub(super) async fn browser_feedback(&self, surface: SurfaceId, action: Action) {
        let Some(browser) = self.pane_registry.borrow().browsers.get(&surface).cloned() else {
            return;
        };
        let ui = &browser.feedback;
        match action {
            Action::Pick => {
                if ui.picking.get() {
                    return;
                }
                if !browser.focus_widget().is_mapped() {
                    ui.panel.set_visible(true);
                    ui.status
                        .set_text("Show this browser tab before selecting an element.");
                    return;
                }
                let operation = match browser.control.admit(None, false) {
                    Ok((operation, _)) => operation,
                    Err(error) => {
                        ui.panel.set_visible(true);
                        ui.status.set_text(&format!(
                            "{error}. Stop and clear the browser task before selecting an element."
                        ));
                        return;
                    }
                };
                let generation = ui.begin();
                gtk::glib::spawn_future_local(async move {
                    let result = pick(&browser, generation).await;
                    drop(operation);
                    if browser.feedback.generation.get() != generation {
                        return;
                    }
                    browser.feedback.stop();
                    match result {
                        Ok(Some(capture)) => browser.feedback.selected(capture),
                        Ok(None) => browser
                            .feedback
                            .status
                            .set_text("Selection cancelled. Your previous request is kept."),
                        Err(error) => browser.feedback.status.set_text(&error),
                    }
                });
            }
            Action::Cancel => {
                ui.stop();
                browser.evaluate_automation_js(STOP, |_| {});
                ui.panel.set_visible(false);
            }
            Action::Targets => {
                let workspace = self
                    .pane_registry
                    .borrow()
                    .surface_workspace
                    .get(&surface)
                    .copied();
                let targets = self
                    .feedback_targets()
                    .await
                    .into_iter()
                    .filter(|(owner, target)| {
                        Some(*owner) == workspace && target.ssh_instance.is_none()
                    })
                    .map(|(_, target)| target)
                    .collect();
                ui.set_targets(targets);
            }
            Action::Copy => match ui.prompt() {
                Ok(prompt) => {
                    ui.panel.clipboard().set_text(&prompt);
                    if let Some(capture) = ui.capture.borrow().as_ref() {
                        capture.retain();
                    }
                    ui.status
                        .set_text("Request copied, including the local screenshot path.");
                }
                Err(error) => ui.status.set_text(&error),
            },
            Action::Send(target) => {
                let result = async {
                    if target.ssh_instance.is_some() {
                        return Err(
                            "Choose a local agent that can read the screenshot file.".into()
                        );
                    }
                    let prompt = ui.prompt()?;
                    let workspace = self
                        .pane_registry
                        .borrow()
                        .surface_workspace
                        .get(&surface)
                        .copied()
                        .ok_or("The browser workspace is no longer available.")?;
                    self.deliver_feedback(workspace, &target, &prompt, || {
                        // A failed submit can still leave a pasted request in the terminal.
                        if let Some(capture) = ui.capture.borrow().as_ref() {
                            capture.retain();
                        }
                    })
                    .await?;
                    Ok::<_, String>(())
                }
                .await;
                match result {
                    Ok(()) => {
                        ui.comment.buffer().set_text("");
                        ui.status.set_text("Request sent to agent. The captured image is kept for the agent to read.");
                    }
                    Err(error) => ui.status.set_text(&error),
                }
            }
        }
    }
}

async fn evaluate(browser: &BrowserPane, source: &str) -> Result<String, String> {
    let (tx, rx) = oneshot::channel();
    browser.evaluate_automation_js(source, move |result| {
        let _ = tx.send(result);
    });
    rx.await
        .map_err(|_| "Browser closed before selection completed".to_string())?
}

async fn pick(browser: &BrowserPane, generation: u64) -> Result<Option<Capture>, String> {
    // Let the selection hint finish laying out before collecting viewport bounds.
    glib::timeout_future(Duration::from_millis(100)).await;
    if browser.feedback.generation.get() != generation {
        return Ok(None);
    }
    let document = browser.control.observation()["document_generation"].clone();
    evaluate(browser, PICK).await?;
    browser.grab_focus();
    let result = async {
        let started = Instant::now();
        loop {
            if browser.feedback.generation.get() != generation
                || !browser.focus_widget().is_mapped()
            {
                return Ok(None);
            }
            if browser.control.observation()["document_generation"] != document {
                return Err("The page navigated. Select the element again.".into());
            }
            if started.elapsed() > Duration::from_secs(120) {
                return Err("Selection timed out. Choose Request change to try again.".into());
            }
            let raw = evaluate(browser, POLL).await?;
            if raw.len() > 32_000 {
                return Err("The element context is too large.".into());
            }
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            if value["cancelled"] == true {
                return Ok(None);
            }
            if let Some(error) = value["error"].as_str() {
                return Err(error.to_string());
            }
            if !value.is_null() {
                let selection: Selection =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                let size = selection.capture_size()?;
                let capture = Capture::new(selection)?;
                // The picker removes its highlight before WebKit captures the image.
                let (tx, rx) = oneshot::channel();
                browser.snapshot_to_png(capture.path(), Some(size), move |result| {
                    let _ = tx.send((result, capture));
                });
                let (result, capture) = glib::future_with_timeout(Duration::from_secs(10), rx)
                    .await
                    .map_err(|_| "The screenshot timed out. Select the element again.".to_string())?
                    .map_err(|_| "The browser closed during capture".to_string())?;
                result?;
                if browser.control.observation()["document_generation"] != document {
                    return Err(
                        "The page navigated during capture. Select the element again.".into(),
                    );
                }
                if evaluate(browser, POLL).await? != raw {
                    return Err("The selected element or viewport changed during capture. Select the element again.".into());
                }
                return Ok(Some(capture));
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
    }
    .await;
    // Do not remove a newer picker started while a previous script was completing.
    if browser.feedback.generation.get() == generation {
        let _ = evaluate(browser, STOP).await;
    }
    result
}
