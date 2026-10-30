// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs only in the opt-in native test executable, on the AppKit main thread.
use super::*;
use crate::ui::browser_pane::BrowserPane;
use flowmux_state::{State, WindowOwner};
use std::io::{BufRead, Read, Write};

mod agent_sessions;
mod cli_paths;
mod cwd_poll;
mod keyboard_focus;
mod notification_cleanup;
mod options_merge;
mod palette;
mod pane_titles;
mod process_args;
mod restored_roots;
mod session_paths;
mod teams_ack;
mod termination_dialog;
mod zoom_reorder;

pub(crate) fn run() {
    let isolated = tempfile::Builder::new()
        .prefix("fm-native-")
        .tempdir_in("/tmp")
        .expect("isolated native smoke directory");
    for (key, directory) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("FLOWMUX_RUNTIME_DIR", "run"),
    ] {
        let path = isolated.path().join(directory);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    // Skill installation must never touch the developer's actual agent folders.
    if std::env::var_os("FLOWMUX_SKILLS_SMOKE_ONLY").is_some() {
        for (key, directory) in [
            ("HOME", "home"),
            ("CLAUDE_CONFIG_DIR", "home/.claude"),
            ("CODEX_HOME", "home/.codex"),
            ("GEMINI_CLI_HOME", "home"),
        ] {
            let path = isolated.path().join(directory);
            std::fs::create_dir_all(&path).unwrap();
            std::env::set_var(key, path);
        }
    }
    std::env::set_var("SHELL", "/bin/sh");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    crate::ui::macos_layer::install_tile_refresh();
    adw::init().expect("native macOS display must be available");
    let app = adw::Application::builder()
        .application_id("com.flowmux.NativeSmoke")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    glib::MainContext::default().block_on(async {
        // Theme and Code Review scenarios now run before the browser/editor/IPC checks.
        // Allow time for the whole suite; individual UI waits remain bounded at 30s.
        glib::future_with_timeout(Duration::from_secs(300), check(&app, isolated.path()))
            .await
            .expect("native smoke exceeded 300 seconds");
    });
    println!("MACOS_NATIVE_SMOKE_OK");
}

async fn wait_until(description: &str, mut ready: impl FnMut() -> bool) {
    glib::future_with_timeout(Duration::from_secs(30), async {
        while !ready() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("native UI timed out: {description}"));
}

async fn javascript(browser: &BrowserPane, source: &str) -> String {
    let (tx, rx) = oneshot::channel();
    browser.evaluate_js(source, move |result| {
        let _ = tx.send(result);
    });
    glib::future_with_timeout(Duration::from_secs(10), rx)
        .await
        .expect("WKWebView JavaScript timed out")
        .unwrap()
        .expect("WKWebView JavaScript failed")
}

async fn check(app: &adw::Application, root: &std::path::Path) {
    crate::ui::browser_control::check_controls();
    crate::keybindings::check_quit_dialog(app).await;
    #[cfg(test)]
    if std::env::var_os("FLOWMUX_AGENT_TEXT_SMOKE_ONLY").is_some() {
        crate::ui::ghostty_pane::check_normal_agent_status_below_cursor().await;
        return;
    }
    if std::env::var_os("FLOWMUX_QUIT_DIALOG_SMOKE_ONLY").is_some() {
        return;
    }
    if std::env::var_os("FLOWMUX_BROWSER_CHROME_SMOKE_ONLY").is_some() {
        crate::ui::browser_chrome::tests::check().await;
        return;
    }
    if std::env::var_os("FLOWMUX_BROWSER_CONTROL_SMOKE_ONLY").is_some() {
        return;
    }
    if std::env::var_os("FLOWMUX_IMAGE_VIEWER_SMOKE_ONLY").is_some() {
        crate::ui::image_viewer::macos::check(app, root).await;
        return;
    }
    if std::env::var_os("FLOWMUX_DMG_UPDATE_SMOKE_ONLY").is_some() {
        crate::ui::update_banner::UpdateBanner::check_native_dmg_update(app.upcast_ref()).await;
        return;
    }
    if std::env::var_os("FLOWMUX_SSH_PREVIEW_SMOKE_ONLY").is_some() {
        WindowController::check_native_ssh_preview_route(app).await;
        BrowserPane::check_native_ssh_preview(app.upcast_ref()).await;
        return;
    }
    if std::env::var_os("FLOWMUX_BROWSER_INSPECTOR_SMOKE_ONLY").is_some() {
        BrowserPane::check_native_inspector(app.upcast_ref()).await;
        return;
    }
    if std::env::var_os("FLOWMUX_CWD_POLL_SMOKE_ONLY").is_some() {
        cwd_poll::check(app, root).await;
        return;
    }
    if std::env::var_os("FLOWMUX_OFFICE_STRESS_SMOKE_ONLY").is_some() {
        crate::ui::agent_office::stress::check().await;
        return;
    }
    if std::env::var_os("FLOWMUX_OFFICE_CACHE_SMOKE_ONLY").is_some() {
        crate::ui::agent_office::check_native_screen_cache(app).await;
        return;
    }
    if std::env::var_os("FLOWMUX_KEYBINDING_ALIAS_SMOKE_ONLY").is_some() {
        crate::ui::keybindings_panel::check_native_aliases(app);
        return;
    }
    if std::env::var_os("FLOWMUX_OPTIONS_MERGE_SMOKE_ONLY").is_some() {
        options_merge::check(app).await;
        return;
    }
    if std::env::var_os("FLOWMUX_NOTIFICATION_CLEANUP_SMOKE_ONLY").is_some() {
        notification_cleanup::check(app).await;
        return;
    }
    if std::env::var_os("FLOWMUX_RESTORED_ROOTS_SMOKE_ONLY").is_some() {
        restored_roots::check(app, root).await;
        return;
    }
    if std::env::var_os("FLOWMUX_UPDATE_LIFETIME_SMOKE_ONLY").is_some() {
        crate::ui::update_banner::UpdateBanner::check_native_lifetime(app.upcast_ref()).await;
        return;
    }
    if std::env::var_os("FLOWMUX_BROWSER_DEADLINE_SMOKE_ONLY").is_some() {
        BrowserPane::check_native_script_deadline(app.upcast_ref()).await;
        return;
    }
    if std::env::var_os("FLOWMUX_BROWSER_LIFETIME_SMOKE_ONLY").is_some() {
        BrowserPane::check_native_lifetime(app.upcast_ref()).await;
        return;
    }
    let owner = WindowOwner::current();
    let store = StateStore::new_lazy_window(State::default(), owner);
    let workspace = store
        .create_workspace(Some("Native smoke".into()), root.into())
        .await;
    let ws = store.get_workspace(workspace).await.unwrap();
    let terminal_pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
    let (bridge, rx) = Bridge::new();
    let controller = WindowController::new(
        app,
        store.clone(),
        Arc::new(ResolvedTheme::load()),
        bridge.clone(),
        gtk::CssProvider::new(),
        None,
    );
    controller.render_workspace(&ws);
    spawn_dispatch_loop(rx, controller.clone());
    controller.window.present();
    wait_until("window mapped", || controller.window.is_mapped()).await;
    if std::env::var_os("FLOWMUX_USAGE_PINS_SMOKE_ONLY").is_some() {
        crate::ui::usage_popover::native_tests::check(&controller.sidebar.usage).await;
        controller.window.destroy();
        options_merge::check(app).await;
        return;
    }
    crate::ui::macos_ime::check_native_surface_access(&controller.window);
    let terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(terminal_pane)
        .unwrap()
        .clone();
    let pid = terminal.pid.get().expect("terminal shell must be running");

    if std::env::var_os("FLOWMUX_SESSION_PATHS_SMOKE_ONLY").is_some() {
        session_paths::check(&controller, terminal_pane, root).await;
        controller.window.destroy();
        return;
    }

    if std::env::var_os("FLOWMUX_EDITOR_FOCUS_SMOKE_ONLY").is_some() {
        crate::keybindings::install_actions(&controller, bridge.clone());
        let pane = controller
            .split_pane(terminal_pane, SplitDirection::Vertical)
            .await
            .unwrap();
        let (_, surface) = store
            .add_editor_surface_to_pane(pane, root.into())
            .await
            .unwrap();
        controller
            .attach_or_rerender_surface(workspace, pane, surface, None)
            .await
            .unwrap();
        let editor = controller.pane_registry.borrow().editors[&surface].clone();
        wait_until("editor mapped", || editor.root.is_mapped()).await;
        editor.flush_pending_changes().await.unwrap();
        editor.check_terminal_focus(&terminal).await;
        editor.grab_focus();
        controller.focus_pane(terminal_pane);
        assert!(
            terminal.widget.is_focus() && !editor.has_native_focus(),
            "mapped pane handoff must finish before the next key event"
        );
        controller.focus_pane(pane);
        assert!(editor.has_native_focus());
        keyboard_focus::check(&controller, workspace, terminal_pane, pane, root).await;
        controller.window.destroy();
        return;
    }

    if std::env::var_os("FLOWMUX_ZOOM_REORDER_SMOKE_ONLY").is_some() {
        zoom_reorder::check(&controller, workspace, terminal_pane).await;
        controller.window.destroy();
        return;
    }
    if std::env::var_os("FLOWMUX_CLI_PATHS_SMOKE_ONLY").is_some() {
        cli_paths::check(&controller, &bridge, root).await;
        controller.window.destroy();
        return;
    }
    if std::env::var_os("FLOWMUX_PALETTE_ORDER_SMOKE_ONLY").is_some() {
        palette::check(&controller, workspace, root).await;
        controller.window.destroy();
        return;
    }
    if std::env::var_os("FLOWMUX_PALETTE_RELEASE_SMOKE_ONLY").is_some() {
        palette::check_release(&controller).await;
        controller.window.destroy();
        return;
    }
    if std::env::var_os("FLOWMUX_AGENT_ENTRYPOINT_SMOKE_ONLY").is_some() {
        process_args::check(&controller, terminal_pane, root).await;
        controller.window.destroy();
        return;
    }
    if std::env::var_os("FLOWMUX_TEAMS_ACK_SMOKE_ONLY").is_some() {
        teams_ack::check(&controller, root).await;
        controller.window.destroy();
        return;
    }

    if std::env::var_os("FLOWMUX_LIVE_TAB_TITLE_SMOKE_ONLY").is_some() {
        pane_titles::check(&controller, workspace, terminal_pane, root).await;
        controller.window.destroy();
        return;
    }

    if std::env::var_os("FLOWMUX_AGENT_SESSION_IO_SMOKE_ONLY").is_some() {
        let surface = ws.surfaces[0]
            .root_pane
            .active_surface_id(terminal_pane)
            .unwrap();
        agent_sessions::check(&store, &bridge, surface).await;
        controller.window.destroy();
        return;
    }

    if std::env::var_os("FLOWMUX_TERMINAL_TEAROFF_SMOKE_ONLY").is_some() {
        controller.check_terminal_tearoff(root).await;
        controller.window.destroy();
        return;
    }

    if std::env::var_os("FLOWMUX_EDITOR_ACK_SMOKE_ONLY").is_some()
        || std::env::var_os("FLOWMUX_EDITOR_POLL_SMOKE_ONLY").is_some()
        || std::env::var_os("FLOWMUX_EDITOR_RECOVERY_SMOKE_ONLY").is_some()
    {
        let (_, surface) = store
            .add_editor_surface_to_pane(terminal_pane, root.into())
            .await
            .unwrap();
        controller
            .attach_or_rerender_surface(workspace, terminal_pane, surface, None)
            .await
            .unwrap();
        let editor = controller.pane_registry.borrow().editors[&surface].clone();
        if std::env::var_os("FLOWMUX_EDITOR_RECOVERY_SMOKE_ONLY").is_some() {
            editor.check_recovery(root).await;
        } else if std::env::var_os("FLOWMUX_EDITOR_POLL_SMOKE_ONLY").is_some() {
            editor.check_poll(root).await;
        } else {
            editor.check_save_ack(root).await;
        }
        controller.window.destroy();
        return;
    }

    if let Some(volume) = std::env::var_os("FLOWMUX_FILE_MOVE_SMOKE_VOLUME") {
        crate::ui::file_browser::macos_smoke::check(
            &controller.window,
            root,
            std::path::Path::new(&volume),
        )
        .await;
        controller.window.destroy();
        return;
    }

    if std::env::var_os("FLOWMUX_AGENT_SMOKE_ONLY").is_some() {
        #[cfg(test)]
        crate::ui::ghostty_pane::check_normal_agent_status_below_cursor().await;
        controller.options.borrow_mut().system_notifications_enabled = false;
        let socket = flowmux_config::paths::runtime_socket_for_pid(std::process::id());
        let handler = Arc::new(crate::ipc_handler::GuiHandler::new(
            flowmux_daemon::DaemonHandler::new(store.clone()),
            bridge.clone(),
        ));
        let server_socket = socket.clone();
        let server = tokio::spawn(async move {
            flowmux_ipc::server::run(&server_socket, handler)
                .await
                .unwrap();
        });
        wait_until("agent IPC listening", || socket.exists()).await;
        let cli = std::env::var_os("FLOWMUX_BUNDLED_CLI_PATH")
            .expect("agent smoke requires the built flowmuxctl");
        fn tooltip_evidence(widget: &gtk::Widget) -> u8 {
            let mut found = 0;
            if widget.widget_name() == "flowmux-agent-attention-summary" {
                if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    if label.label().contains("Input ") {
                        found |= 16;
                    }
                    if label.label().contains("Waiting ") {
                        found |= 32;
                    }
                }
            }
            if let Some(text) = widget.tooltip_text() {
                if text.lines().any(|line| line == "Hook-reported state") {
                    found |= 1;
                }
                if text.lines().any(|line| line == "Screen-inferred state") {
                    found |= 2;
                }
                if text.contains("waiting on user") {
                    found |= 4;
                }
                if text.contains("retry inferred from screen") {
                    found |= 8;
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                found |= tooltip_evidence(&widget);
                child = widget.next_sibling();
            }
            found
        }
        let evidence_seen = Rc::new(std::cell::Cell::new(0u8));
        let observed_evidence = evidence_seen.clone();
        let observed_window = controller.window.downgrade();
        let tooltip_observer = glib::timeout_add_local(Duration::from_millis(50), move || {
            if let Some(window) = observed_window.upgrade() {
                observed_evidence
                    .set(observed_evidence.get() | tooltip_evidence(window.upcast_ref()));
            }
            glib::ControlFlow::Continue
        });
        let output = tokio::process::Command::new("python3")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../scripts/test-agent-hooks-gui.py"),
            )
            .args(["--socket", socket.to_str().unwrap(), "--cli"])
            .arg(cli)
            .kill_on_drop(true)
            .output();
        let output = glib::future_with_timeout(Duration::from_secs(180), output)
            .await
            .expect("agent hook replay timed out")
            .unwrap();
        tooltip_observer.remove();
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "agent hook replay failed");
        assert!(String::from_utf8_lossy(&output.stdout).contains("LIVE_NATIVE_HOOK_MATRIX_OK"));
        assert_eq!(
            evidence_seen.get(),
            63,
            "live Agent rows must expose evidence and separate input/waiting counts"
        );
        println!("MACOS_AGENT_EVIDENCE_TOOLTIPS_OK");
        crate::ui::sidebar::check_stable_activity_rows().await;
        println!("MACOS_STABLE_ACTIVITY_ROWS_OK");
        crate::ui::agent_bar::check_stable_agent_bar_items().await;
        println!("MACOS_STABLE_AGENT_BAR_OK");
        server.abort();
        controller.window.destroy();
        println!("MACOS_NATIVE_AGENT_HOOKS_OK");
        return;
    }

    if std::env::var_os("FLOWMUX_SKILLS_SMOKE_ONLY").is_some() {
        check_skills(&controller).await;
        controller.window.destroy();
        return;
    }

    let review_only = std::env::var_os("FLOWMUX_REVIEW_SMOKE_ONLY").is_some();
    if review_only {
        check_sidebar_footer(&controller).await;
    }
    println!("MACOS_MENU_TOGGLE_START");
    crate::ui::popover_pos::menu_toggle_smoke().await;
    for _ in 0..2 {
        controller.dispatch(GtkCommand::ShowOptionsDialog).await;
        let options = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .filter(|w| {
                w.widget_name() == "flowmux-options-dialog"
                    && w.transient_for().as_ref() == Some(controller.window.upcast_ref())
            })
            .collect::<Vec<_>>();
        assert_eq!(options.len(), 1);
        controller.dispatch(GtkCommand::ShowOptionsDialog).await;
        assert!(!options[0].is_visible());
    }
    println!("OPTIONS_BUTTON_TOGGLE_OK");

    if review_only {
        crate::ui::review_window::smoke(&controller.window).await;
        super::review::handoff_smoke(&controller).await;
        super::review::pane_scope_smoke(&controller).await;
        controller.window.destroy();
        return;
    }

    termination_dialog::check(&controller, workspace, terminal_pane).await;
    check_theme_focus(&controller).await;
    check_theme_sources(&controller).await;
    crate::ui::review_window::smoke(&controller.window).await;
    super::review::handoff_smoke(&controller).await;

    println!("MACOS_NATIVE_BROWSER_START");
    // A loopback fixture avoids file URL access differences between macOS versions.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let _idle_connection = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let page = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            // A speculative connection can close before its socket is configured.
            if stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .is_err()
                || stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .is_err()
            {
                continue;
            }
            // WebKit may preconnect without sending a request on that socket.
            let complete = std::io::BufReader::new((&stream).take(8192))
                .lines()
                .map_while(Result::ok)
                .any(|line| line.is_empty());
            if !complete {
                continue;
            }
            let body = "<!doctype html><title>Native smoke</title><input id=smoke>";
            if write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).is_ok() {
                break;
            }
        }
    });
    let (ack, opened) = oneshot::channel();
    bridge
        .tx
        .send(GtkCommand::BrowserOpenSplit {
            target_pane: Some(terminal_pane),
            url,
            direction: SplitDirection::Vertical,
            ack,
        })
        .await
        .unwrap();
    let pane = opened.await.unwrap().unwrap().pane;
    let browser = controller
        .pane_registry
        .borrow()
        .active_browser(pane)
        .unwrap()
        .clone();
    wait_until("browser loaded", || {
        browser.current_title() == "Native smoke"
    })
    .await;
    page.join().unwrap();
    if std::env::var_os("FLOWMUX_TAB_DRAG_PREVIEW_SMOKE_ONLY").is_some() {
        javascript(
            &browser,
            "document.body.style.background='#18394b'; 'ready'",
        )
        .await;
    }
    crate::ui::workspace_view::tab_drag_smoke::check(
        controller.window.upcast_ref(),
        &controller.pane_registry,
        terminal_pane,
        pane,
    )
    .await;
    if std::env::var_os("FLOWMUX_TAB_DRAG_PREVIEW_SMOKE_ONLY").is_some() {
        controller.window.destroy();
        return;
    }
    assert_eq!(
        javascript(
            &browser,
            "document.getElementById('smoke').value='retained'; window.smoke=42; 'ready'"
        )
        .await,
        "ready"
    );
    browser.grab_focus();
    assert_eq!(
        javascript(
            &browser,
            "document.getElementById('smoke').focus(); document.activeElement.id"
        )
        .await,
        "smoke"
    );
    controller.rerender_workspace(&store.get_workspace(workspace).await.unwrap());
    assert_eq!(
        javascript(
            &browser,
            "JSON.stringify([window.smoke, document.getElementById('smoke').value])"
        )
        .await,
        "[42,\"retained\"]"
    );
    assert_eq!(
        controller
            .pane_registry
            .borrow()
            .active_terminal(terminal_pane)
            .unwrap()
            .pid
            .get(),
        Some(pid)
    );
    // Without WKUIDelegate panels, WebKit silently cancels confirm() and prompt().
    javascript(
        &browser,
        "window.dialogs = []; setTimeout(() => dialogs.push(alert('Notice'), confirm('Proceed?'), prompt('Name?', 'preset'), confirm('Cancel confirm'), prompt('Cancel prompt', 'preset'), confirm('Close confirm'), prompt('Close prompt', 'preset')), 0); 'scheduled'",
    )
    .await;
    for (message, response) in [
        ("Notice", Some("ok")),
        ("Proceed?", Some("ok")),
        ("Name?", Some("ok")),
        ("Cancel confirm", Some("cancel")),
        ("Cancel prompt", Some("cancel")),
        ("Close confirm", None),
        ("Close prompt", None),
    ] {
        wait_until("page dialog shown", || {
            controller.window.visible_dialog().is_some_and(|dialog| {
                dialog
                    .downcast::<adw::AlertDialog>()
                    .is_ok_and(|alert| alert.body() == message)
            })
        })
        .await;
        let dialog = controller
            .window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        crate::ui::browser_pane::stills_smoke::check(controller.window.upcast_ref(), true, false)
            .await;
        if message == "Name?" {
            let entry = dialog
                .extra_child()
                .unwrap()
                .downcast::<gtk::Entry>()
                .unwrap();
            assert_eq!(entry.text(), "preset");
            entry.set_text("수정한 이름");
        }
        if let Some(response) = response {
            dialog.emit_by_name::<()>("response", &[&response]);
        }
        dialog.close();
    }
    assert_eq!(
        javascript(&browser, "JSON.stringify(dialogs)").await,
        "[null,true,\"수정한 이름\",false,null,false,null]"
    );
    assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    crate::ui::browser_pane::stills_smoke::check(controller.window.upcast_ref(), false, false)
        .await;
    let closing_page = root.join("closing-dialog.html");
    std::fs::write(
        &closing_page,
        "<!doctype html><title>Closing dialog</title>",
    )
    .unwrap();
    for (source, expected) in [
        ("confirm('Close window')", "false"),
        ("prompt('Close window', 'preset')", "null"),
    ] {
        let closing_browser = BrowserPane::new(
            PaneId::new(),
            SurfaceId::new(),
            Some(&format!("file://{}", closing_page.display())),
            crate::ui::pane_terminal::PaneCallbacks::noop_for_test(),
            flowmux_config::options::BrowserEngine::Webkit,
            false,
        );
        let closing_window = adw::ApplicationWindow::builder()
            .application(app)
            .content(&closing_browser.root)
            .default_width(640)
            .default_height(480)
            .build();
        closing_window.present();
        wait_until("closing browser mapped", || {
            closing_browser.root.is_mapped() && closing_browser.current_title() == "Closing dialog"
        })
        .await;
        javascript(
            &closing_browser,
            &format!("setTimeout(() => window.reply = {source}, 0); 'scheduled'"),
        )
        .await;
        wait_until("page dialog before window close", || {
            closing_window.visible_dialog().is_some()
        })
        .await;
        closing_window.destroy();
        println!("MACOS_NATIVE_BROWSER_WINDOW_CLOSED {source}");
        let reply = glib::future_with_timeout(Duration::from_secs(10), async {
            loop {
                let reply = javascript(&closing_browser, "JSON.stringify(window.reply)").await;
                if !reply.is_empty() {
                    break reply;
                }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("window close must answer the pending page dialog");
        assert_eq!(reply, expected);
        assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    }
    println!("MACOS_NATIVE_BROWSER_DIALOGS_OK");
    println!("MACOS_NATIVE_BROWSER_OK");

    let file = root.join("unsaved.txt");
    std::fs::write(&file, "original\n").unwrap();
    let (_, surface) = store
        .add_editor_surface_to_pane(pane, root.into())
        .await
        .unwrap();
    controller
        .attach_or_rerender_surface(workspace, pane, surface, None)
        .await
        .unwrap();
    let editor = controller.pane_registry.borrow().editors[&surface].clone();
    editor.open_file(&file).await.unwrap();
    editor.flush_pending_changes().await.unwrap();
    editor.grab_focus();
    wait_until("editor focused", || editor.has_native_focus()).await;
    editor.insert_smoke_text("unsaved smoke");
    wait_until("editor dirty", || !editor.dirty_document_paths().is_empty()).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original\n");
    controller.focus_pane(terminal_pane);
    assert!(
        terminal.widget.is_focus() && !editor.has_native_focus(),
        "mapped pane handoff must finish before the next key event"
    );
    wait_until("terminal focused", || !editor.has_native_focus()).await;
    controller.focus_pane(pane);
    wait_until("editor refocused", || editor.has_native_focus()).await;
    editor.check_terminal_focus(&terminal).await;
    editor.grab_focus();
    println!("MACOS_NATIVE_FOCUS_OK");

    // A hook-owned presence needs a real agent in this terminal's process tree.
    // Otherwise the normal process sweep correctly removes this test report.
    let agent_source = root.join("native_agent.c");
    let agent_executable = root.join("claude");
    std::fs::write(
        &agent_source,
        "#include <unistd.h>\nint main(void) { alarm(120); for (;;) pause(); }\n",
    )
    .unwrap();
    let compiled = std::process::Command::new("cc")
        .arg(&agent_source)
        .arg("-o")
        .arg(&agent_executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    terminal
        .write_input(
            format!(
                "exec {}\n",
                flowmux_core::ssh::shell_quote(agent_executable.to_str().unwrap())
            )
            .as_bytes(),
        )
        .unwrap();
    let terminal_pid = u32::try_from(pid).unwrap();
    wait_until("agent process running", || {
        flowmux_procmon::agent_names_in_tree(terminal_pid).contains(&"claude")
    })
    .await;
    let agent_pid = flowmux_procmon::descendants(terminal_pid)
        .unwrap()
        .into_iter()
        .find(|pid| flowmux_procmon::agent_name_for_pid(*pid) == Some("claude"))
        .unwrap();

    controller.window.close();
    wait_until("dirty dialog presented", || {
        controller.window.visible_dialog().is_some()
    })
    .await;
    let dialog = controller
        .window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert_eq!(
        dialog.heading().as_deref(),
        Some("Save changes before closing?")
    );
    crate::ui::browser_pane::stills_smoke::check(controller.window.upcast_ref(), true, true).await;
    // Exercise the real socket handler and GTK dispatcher while CloseWindow
    // waits for the dirty-editor decision. Saturating the ordinary endpoint
    // must not hide hook telemetry or prevent state queries.
    // This isolated smoke verifies in-app delivery, without requiring a desktop
    // notification service (which can wait on D-Bus authentication on CI).
    controller.options.borrow_mut().system_notifications_enabled = false;
    let socket = root.join("run/native.sock");
    let server_socket = socket.clone();
    let handler = Arc::new(crate::ipc_handler::GuiHandler::new(
        flowmux_daemon::DaemonHandler::new(store.clone()),
        bridge.clone(),
    ));
    let ipc_server = tokio::spawn(async move {
        flowmux_ipc::server::run(&server_socket, handler)
            .await
            .unwrap();
    });
    wait_until("IPC listening", || socket.exists()).await;
    let agent_surface = controller
        .pane_registry
        .borrow()
        .active_surface(terminal_pane)
        .unwrap();
    let hooks = tokio::spawn(async move {
        use flowmux_ipc::{
            client::Client,
            protocol::{AgentLifecycleEvent, Envelope, Payload, Request, Response},
        };
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let mut held = Vec::new();
        for _ in 0..64 {
            let mut stream =
                tokio::io::BufReader::new(tokio::net::UnixStream::connect(&socket).await.unwrap());
            stream
                .get_mut()
                .write_all(b"{\"id\":1,\"kind\":\"request\",\"verb\":\"ping\"}\n")
                .await
                .unwrap();
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let reply: Envelope = serde_json::from_str(&line).unwrap();
            assert!(matches!(reply.payload, Payload::Response(Response::Pong)));
            held.push(stream);
        }
        println!("MACOS_NATIVE_IPC_CONNECTIONS_SATURATED");
        let client = Client::connect(&socket).await.unwrap();
        for request in [
            Request::AgentLifecycleUpdate {
                notification: None,
                pane: Some(terminal_pane),
                surface: agent_surface,
                agent: "claude".into(),
                pid: Some(agent_pid),
                seq: Some(1),
                session_id: "native-saturation".into(),
                lifecycle: AgentLifecycleEvent::TurnStarted {
                    turn_id: None,
                    status_text: "running".into(),
                },
            },
            Request::Notify {
                pane: Some(terminal_pane),
                surface: Some(agent_surface),
                title: "Saturation hook".into(),
                body: "Still delivered while closing".into(),
                level: flowmux_core::NotificationLevel::NeedsInput,
            },
        ] {
            println!("MACOS_NATIVE_IPC_REQUEST {request:?}");
            assert!(matches!(client.call(request).await.unwrap(), Response::Ok));
            println!("MACOS_NATIVE_IPC_REQUEST_OK");
        }
        (held, client)
    });
    // This batch includes first-time native hook/notification rendering. Match
    // the other native UI readiness checks; no request is retried.
    let (held, client) = glib::future_with_timeout(Duration::from_secs(30), hooks)
        .await
        .expect("hooks must respond during saturated close confirmation")
        .unwrap();
    // That first rendering has held the main thread for 10 to 18 seconds on
    // CI runners, starting up to a few hundred milliseconds after the hooks,
    // which is past a query's server-side deadline. Let it finish first: a
    // blocked main loop cannot complete this wait.
    glib::timeout_future(Duration::from_secs(1)).await;
    let queries = tokio::spawn(async move {
        use flowmux_ipc::protocol::{Request, Response};
        assert!(matches!(
            client.call(Request::Ping).await.unwrap(),
            Response::Pong
        ));
        assert!(matches!(
            client
                .call(Request::PaneReadScreen {
                    pane: terminal_pane
                })
                .await
                .unwrap(),
            Response::ScreenContents { .. }
        ));
        let response = client
            .call(Request::NotificationsList { unread_only: false })
            .await
            .unwrap();
        assert!(
            matches!(&response, Response::Notifications { entries, .. } if entries.iter().any(|entry| entry.title == "Saturation hook")),
            "{response:?}"
        );
        // The ordinary endpoint stays saturated until the queries are answered.
        drop(held);
    });
    // Individual queries retain their server-side deadline.
    glib::future_with_timeout(Duration::from_secs(30), queries)
        .await
        .expect("queries must respond during saturated close confirmation")
        .unwrap();
    assert!(controller.window.visible_dialog().is_some());
    assert!(!controller.window_close.approved.get());
    controller.poll_agent_processes().await;
    let presence = store
        .located_agent_presence(agent_surface)
        .await
        .unwrap()
        .presence;
    assert_eq!(presence.name, "claude");
    assert_eq!(presence.pid, Some(agent_pid));
    assert_eq!(presence.source.as_deref(), Some("flowmux:hook"));
    assert_eq!(presence.session_id.as_deref(), Some("native-saturation"));
    assert!(controller
        .notifications
        .entries()
        .iter()
        .any(|entry| entry.title == "Saturation hook"));
    ipc_server.abort();
    println!("MACOS_NATIVE_IPC_SATURATION_OK");

    // A valid maximum-length socket must still serve the GUI when appending
    // the companion suffix exceeds macOS's sockaddr_un path limit.
    let socket = root.join("l".repeat(103 - root.as_os_str().len() - 1));
    let server_socket = socket.clone();
    let handler = Arc::new(crate::ipc_handler::GuiHandler::new(
        flowmux_daemon::DaemonHandler::new(store.clone()),
        bridge.clone(),
    ));
    let ipc_server = tokio::spawn(async move {
        flowmux_ipc::server::run(&server_socket, handler)
            .await
            .unwrap();
    });
    wait_until("long IPC path listening", || socket.exists()).await;
    let query = tokio::spawn(async move {
        let client = flowmux_ipc::client::Client::connect(&socket).await.unwrap();
        client
            .call(flowmux_ipc::Request::PaneReadScreen {
                pane: terminal_pane,
            })
            .await
            .unwrap()
    });
    assert!(matches!(
        glib::future_with_timeout(Duration::from_secs(30), query)
            .await
            .unwrap()
            .unwrap(),
        flowmux_ipc::Response::ScreenContents { .. }
    ));
    ipc_server.abort();
    println!("MACOS_NATIVE_IPC_LONG_PATH_OK");

    assert!(dialog.close());
    wait_until("dirty dialog closed", || {
        !controller.window_close.prompting.get() && controller.window.visible_dialog().is_none()
    })
    .await;
    assert!(controller.window.is_visible());
    assert!(!editor.dirty_document_paths().is_empty());
    assert_eq!(terminal.pid.get(), Some(pid));
    println!("MACOS_NATIVE_CLOSE_CANCEL_OK");
    crate::ui::browser_pane::stills_smoke::check(controller.window.upcast_ref(), false, false)
        .await;
    println!("MACOS_NATIVE_DIALOG_STILLS_OK");

    editor.save_all_dirty().await.unwrap();
    wait_until("editor saved", || editor.dirty_document_paths().is_empty()).await;
    let state_path = flowmux_state::default_path().unwrap();
    if state_path.exists() {
        std::fs::remove_file(&state_path).unwrap();
    }
    std::fs::create_dir_all(&state_path).unwrap();
    controller.window.close();
    wait_until("save error presented", || {
        controller.window.visible_dialog().is_some()
    })
    .await;
    let dialog = controller
        .window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert_eq!(dialog.heading().as_deref(), Some("Could not save session"));
    assert!(controller.window.is_visible());
    assert!(!controller.window_close.approved.get());
    let (ack, screen) = oneshot::channel();
    bridge
        .send(GtkCommand::PaneReadScreen {
            pane: terminal_pane,
            ack,
        })
        .await
        .unwrap();
    assert!(glib::future_with_timeout(Duration::from_secs(30), screen)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
    std::fs::remove_dir(&state_path).unwrap();
    assert!(dialog.close());
    wait_until("save error closed", || {
        !controller.window_close.prompting.get() && controller.window.visible_dialog().is_none()
    })
    .await;
    controller.window.close();
    wait_until("window closed", || {
        controller.window_close.approved.get() && !controller.window.is_visible()
    })
    .await;
    let saved = flowmux_state::load().unwrap();
    assert!(saved
        .windows
        .iter()
        .any(|window| window.instance_id == owner.instance_id));
    println!("MACOS_NATIVE_SAVE_RETRY_OK");
    cwd_poll::check(app, root).await;
    BrowserPane::check_native_inspector(app.upcast_ref()).await;
}

async fn check_sidebar_footer(controller: &WindowController) {
    let footer = controller.sidebar.root.last_child().unwrap();
    let scroll = footer
        .last_child()
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    let actions = scroll.child().unwrap().first_child().unwrap();
    let buttons: Vec<_> =
        std::iter::successors(actions.first_child(), |w| w.next_sibling()).collect();
    let usage = buttons
        .iter()
        .position(|w| w.widget_name() == "flowmux-usage-button")
        .unwrap();
    let diff = buttons[usage + 1]
        .clone()
        .downcast::<gtk::Button>()
        .unwrap();
    assert_eq!(diff.widget_name(), "flowmux-diff-button");
    assert_eq!(diff.action_name().as_deref(), Some("win.open-diff-review"));
    let app = controller
        .window
        .application()
        .unwrap()
        .downcast::<adw::Application>()
        .unwrap();
    crate::keybindings::install_accels(&app, &flowmux_config::options::Options::default());
    assert_eq!(
        app.accels_for_action("win.open-diff-review"),
        ["<Control><Alt>e"]
    );
    let previous = controller.sidebar_split.position();
    let adjustment = scroll.hadjustment();
    controller.sidebar_split.set_position(210);
    wait_until("footer overflows at right edge", || {
        adjustment.upper() > adjustment.page_size()
            && (adjustment.value() - (adjustment.upper() - adjustment.page_size())).abs() < 1.0
    })
    .await;
    // Adjustment values can update before GTK allocates the scrolled children.
    wait_until(
        "footer clips left icons and keeps the last icon visible",
        || {
            let first = buttons[0].compute_bounds(&scroll).unwrap();
            let last = buttons.last().unwrap().compute_bounds(&scroll).unwrap();
            first.x() < 0.0
                && last.x() >= 0.0
                && last.x() + last.width() <= scroll.width() as f32 + 1.0
        },
    )
    .await;
    adjustment.set_value(0.0);
    glib::timeout_future(Duration::from_millis(100)).await;
    assert_eq!(adjustment.value(), 0.0, "manual scroll must not snap back");
    controller.sidebar_split.set_position(600);
    wait_until("footer expands", || scroll.width() > 500).await;
    controller.sidebar_split.set_position(210);
    wait_until("footer resize restores right edge", || {
        scroll.width() < 300
            && (adjustment.value() - (adjustment.upper() - adjustment.page_size())).abs() < 1.0
    })
    .await;
    controller.sidebar_split.set_position(previous);
    println!("DIFF_REVIEW_SIDEBAR_FOOTER_OK");
}

async fn check_theme_focus(controller: &WindowController) {
    use flowmux_config::options::Options;
    let display = gtk::gdk::Display::default().unwrap();
    gtk::style_context_add_provider_for_display(
        &display,
        &controller.css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let badge = gtk::Label::new(Some("Focused pane"));
    badge.add_css_class("flowmux-pane-zoom-badge");
    let window = gtk::Window::builder()
        .title("Theme focus verification")
        .default_width(360)
        .default_height(100)
        .child(&badge)
        .build();
    window.present();
    wait_until("theme focus window mapped", || window.is_mapped()).await;
    for (id, color) in [
        ("github-light", "#175cd3"),
        ("catppuccin-latte", "#175cd3"),
        ("solarized-light", "#175cd3"),
        ("one-dark", "#fff4b3"),
    ] {
        let opts = Options {
            theme: Some(id.into()),
            ..Options::default()
        };
        controller.apply_runtime_theme(&opts);
        glib::timeout_future(Duration::from_millis(100)).await;
        assert_eq!(badge.color(), gtk::gdk::RGBA::parse(color).unwrap());
        assert_eq!(controller.current_theme().is_dark(), id == "one-dark");
    }
    let custom = Options {
        theme: Some("github-light".into()),
        ..Options::default()
    }
    .with_focus_border_color("#123456")
    .with_focus_border_opacity(42);
    controller.apply_runtime_theme(&custom);
    glib::timeout_future(Duration::from_millis(100)).await;
    assert_eq!(badge.color(), gtk::gdk::RGBA::parse("#123456").unwrap());
    controller.apply_runtime_theme(&Options::default());
    window.close();
    println!("MACOS_NATIVE_THEME_FOCUS_OK");
}

fn theme_widget<T: IsA<gtk::Widget> + glib::object::IsClass>(root: &gtk::Widget, name: &str) -> T {
    fn find(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if root.widget_name() == name {
            return Some(root.clone());
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            if let Some(found) = find(&widget, name) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }
    find(root, name)
        .expect("theme widget exists")
        .downcast::<T>()
        .expect("theme widget type")
}

async fn check_theme_sources(controller: &WindowController) {
    use crate::ui::theme_tab::{self, ThemeSelection};
    use flowmux_config::options::Options;
    use vte::prelude::*;
    let path = flowmux_config::theme::user_theme_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "background = #102030\nforeground = #e0e0e0\n").unwrap();
    let state = Rc::new(RefCell::new(ThemeSelection::default()));
    let changed = Rc::new(Cell::new(0));
    let apply = {
        let state = state.clone();
        let changed = changed.clone();
        let controller = controller.clone();
        Rc::new(move || {
            let selection = state.borrow();
            let opts = Options {
                theme: selection.theme.clone(),
                theme_overrides: selection.overrides.clone(),
                ..Options::default()
            };
            flowmux_config::options::save(&opts).unwrap();
            controller.apply_runtime_theme(&opts);
            changed.set(changed.get() + 1);
        })
    };
    let tab = theme_tab::build(state.clone(), apply);
    let window = adw::Window::builder()
        .title("Theme source verification")
        .default_width(660)
        .default_height(720)
        .content(&tab)
        .build();
    window.present();
    wait_until("theme source window mapped", || window.is_mapped()).await;
    let list: gtk::ListBox = theme_widget(&tab, "flowmux-theme-list");
    let preview: vte::Terminal = theme_widget(&tab, "flowmux-theme-preview");
    assert!(preview.pty().is_none(), "preview must never spawn a shell");
    wait_until("preview sample rendered", || {
        preview
            .text_format(vte::Format::Text)
            .is_some_and(|text| text.contains("Selected text") && text.contains("fn main()"))
    })
    .await;
    assert_eq!(list.selected_row().unwrap().index(), 0);
    assert_eq!(changed.get(), 0);
    for (row, source, bg) in [
        (3, Some("dracula"), "#282a36"),
        (1, Some("default"), "#282c34"),
        (0, None, "#102030"),
    ] {
        list.select_row(list.row_at_index(row).as_ref());
        glib::timeout_future(Duration::from_millis(80)).await;
        assert_eq!(state.borrow().theme.as_deref(), source);
        assert_eq!(flowmux_config::options::load().theme.as_deref(), source);
        assert_eq!(
            controller.current_theme().bg,
            gtk::gdk::RGBA::parse(bg).unwrap()
        );
        assert_eq!(
            preview.color_background_for_draw(),
            controller.current_theme().bg
        );
    }
    assert_eq!(changed.get(), 3);
    let reopened = theme_tab::build(
        state.clone(),
        Rc::new(|| panic!("initial selection must not save")),
    );
    let reopened_list: gtk::ListBox = theme_widget(&reopened, "flowmux-theme-list");
    assert_eq!(reopened_list.selected_row().unwrap().index(), 0);
    let background: gtk::ColorDialogButton = theme_widget(&tab, "flowmux-theme-color-0");
    let foreground: gtk::ColorDialogButton = theme_widget(&tab, "flowmux-theme-color-1");
    let cursor: gtk::ColorDialogButton = theme_widget(&tab, "flowmux-theme-color-2");
    let reset_background: gtk::Button = theme_widget(&tab, "flowmux-theme-reset-0");
    let reset_foreground: gtk::Button = theme_widget(&tab, "flowmux-theme-reset-1");
    let reset_all: gtk::Button = theme_widget(&tab, "flowmux-theme-reset-all");
    let status: gtk::Label = theme_widget(&tab, "flowmux-theme-override-status");
    assert_eq!(status.text(), "Using theme colors");
    assert!(!reset_all.is_sensitive());
    background.set_rgba(&gtk::gdk::RGBA::parse("#112233").unwrap());
    foreground.set_rgba(&gtk::gdk::RGBA::parse("#eeeeee").unwrap());
    assert_eq!(
        cursor.rgba(),
        foreground.rgba(),
        "inherited cursor follows text override"
    );
    assert!(reset_background.is_sensitive() && reset_foreground.is_sensitive());
    assert!(status.text().contains("active: 2"));
    list.select_row(list.row_at_index(10).as_ref()); // GitHub Light
    assert_eq!(controller.current_theme().bg, background.rgba());
    assert!(status.text().contains("active: 2"));
    let before_reset = changed.get();
    reset_background.emit_clicked();
    assert_eq!(changed.get(), before_reset + 1, "reset saves once");
    assert_eq!(controller.current_theme().bg, gtk::gdk::RGBA::WHITE);
    assert_eq!(controller.current_theme().fg, foreground.rgba());
    assert!(state.borrow().overrides.background.is_none());
    assert!(state.borrow().overrides.foreground.is_some());
    assert!(!reset_background.is_sensitive() && reset_foreground.is_sensitive());
    assert!(status.text().contains("active: 1"));
    reset_all.emit_clicked();
    assert!(state.borrow().overrides.is_empty());
    assert!(flowmux_config::options::load().theme_overrides.is_empty());
    assert!(!reset_all.is_sensitive());
    assert_eq!(status.text(), "Using theme colors");
    assert_eq!(
        controller.current_theme().fg,
        gtk::gdk::RGBA::parse("#24292f").unwrap()
    );
    glib::timeout_future(Duration::from_millis(400)).await;
    assert_eq!(preview.color_background_for_draw(), gtk::gdk::RGBA::WHITE);
    let styled_preview = preview.text_format(vte::Format::Html).unwrap();
    assert!(styled_preview.contains("<font color=\"#116329\">PASS</font>"));
    assert!(styled_preview.contains("<font color=\"#CF222E\">error:</font>"));
    assert!(styled_preview.contains("background-color:#ADD6FF"));
    save_theme_snapshot(window.upcast_ref(), "light");
    list.select_row(list.row_at_index(3).as_ref());
    glib::timeout_future(Duration::from_millis(400)).await;
    assert_eq!(
        preview.color_background_for_draw(),
        controller.current_theme().bg
    );
    save_theme_snapshot(window.upcast_ref(), "dark");
    assert!(preview
        .text_format(vte::Format::Text)
        .unwrap()
        .contains("error: example diagnostic"));
    println!("MACOS_NATIVE_THEME_PREVIEW_OK");
    println!("MACOS_NATIVE_THEME_OVERRIDES_OK");
    for (index, preset) in flowmux_config::presets::PRESETS.iter().enumerate() {
        list.select_row(list.row_at_index(index as i32 + 1).as_ref());
        glib::timeout_future(Duration::from_millis(120)).await;
        let resolved = controller.current_theme();
        assert_eq!(
            flowmux_config::options::load().theme.as_deref(),
            Some(preset.id)
        );
        assert_eq!(preview.color_background_for_draw(), resolved.bg);
        let text = preview.text_format(vte::Format::Text).unwrap();
        assert!(
            text.contains("00 Aa") && text.contains("15 Aa"),
            "{}: clipped ANSI preview",
            preset.id
        );
        let html = preview.text_format(vte::Format::Html).unwrap();
        for color in &resolved.palette {
            if *color != resolved.fg {
                let hex = format!(
                    "#{:02X}{:02X}{:02X}",
                    (color.red() * 255.0).round() as u8,
                    (color.green() * 255.0).round() as u8,
                    (color.blue() * 255.0).round() as u8
                );
                assert!(
                    html.contains(&hex),
                    "{}: ANSI color {hex} missing",
                    preset.id
                );
            }
        }
        if matches!(
            preset.id,
            "github-dark"
                | "gruvbox-light"
                | "flowmux-contrast-dark"
                | "flowmux-contrast-light"
                | "catppuccin-latte"
                | "solarized-dark"
                | "solarized-light"
        ) {
            glib::timeout_future(Duration::from_millis(300)).await;
            save_theme_snapshot(window.upcast_ref(), preset.id);
            save_theme_snapshot(preview.upcast_ref(), &format!("{}-palette", preset.id));
        }
        println!("MACOS_NATIVE_PALETTE_OK {}", preset.id);
    }
    window.close();
    std::fs::remove_file(path).unwrap();
    flowmux_config::options::save(&Options::default()).unwrap();
    controller.apply_runtime_theme(&Options::default());
    println!("MACOS_NATIVE_THEME_SOURCES_OK");
}

fn save_theme_snapshot(widget: &gtk::Widget, name: &str) {
    let Some(directory) = std::env::var_os("FLOWMUX_THEME_SNAPSHOT_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(widget)).snapshot(
        &snapshot,
        widget.width() as f64,
        widget.height() as f64,
    );
    let node = snapshot.to_node().expect("mapped theme tab renders");
    widget
        .native()
        .unwrap()
        .renderer()
        .unwrap()
        .render_texture(&node, None)
        .save_to_png(directory.join(format!("theme-{name}.png")))
        .unwrap();
}

async fn check_skills(controller: &WindowController) {
    use flowmux_cli::agent::{self, Skill, SkillOverrides, Target};
    let home = agent::resolved_home().unwrap();
    let overrides = SkillOverrides::from_env();
    let existing = Skill::Browser.path(Target::ClaudeCode, &home, None, &overrides);
    std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
    std::fs::write(&existing, "older bundled skill").unwrap();
    let applied = flowmux_config::paths::state_dir()
        .unwrap()
        .join("installed-skill-version");
    std::fs::create_dir_all(applied.parent().unwrap()).unwrap();
    std::fs::write(&applied, "999.0.0").unwrap();
    crate::update::skills::refresh_installed().unwrap();
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "older bundled skill"
    );
    std::fs::remove_file(&applied).unwrap();
    crate::update::skills::refresh_installed().unwrap();
    assert_eq!(Skill::Browser.doctor(&existing), agent::DoctorStatus::Ok);
    assert_eq!(
        std::fs::read_to_string(&applied).unwrap(),
        env!("CARGO_PKG_VERSION")
    );
    assert!(!Skill::Browser
        .path(Target::Codex, &home, None, &overrides)
        .exists());
    assert!(std::fs::read_dir(existing.parent().unwrap())
        .unwrap()
        .flatten()
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with("SKILL.md.flowmux-backup-")
            && std::fs::read_to_string(entry.path()).unwrap() == "older bundled skill"));
    std::fs::remove_dir_all(existing.parent().unwrap()).unwrap();
    println!("MACOS_NATIVE_SKILLS_AUTO_UPDATE_OK");
    let legacy = agent::resolved_codex_home()
        .unwrap()
        .join("skills/flowmux-browser/SKILL.md");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "unmanaged copy").unwrap();
    controller.dispatch(GtkCommand::ShowOptionsDialog).await;
    let dialog = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.widget_name() == "flowmux-options-dialog" && w.is_visible())
        .unwrap();
    let stack: adw::ViewStack = theme_widget(dialog.upcast_ref(), "flowmux-options-stack");
    stack.set_visible_child_name("skills");
    let reset: gtk::Button = theme_widget(dialog.upcast_ref(), "flowmux-options-reset");
    assert!(
        !reset.is_visible(),
        "general reset must not look like skill removal"
    );
    let refresh: gtk::Button = theme_widget(dialog.upcast_ref(), "flowmux-skills-refresh");
    let preview: gtk::Expander = theme_widget(dialog.upcast_ref(), "flowmux-skill-preview");
    let manual: gtk::TextView = theme_widget(&preview.child().unwrap(), "flowmux-skill-contents");
    assert!(!manual.is_editable());
    assert_eq!(
        manual
            .buffer()
            .text(
                &manual.buffer().start_iter(),
                &manual.buffer().end_iter(),
                false
            )
            .as_str(),
        Target::payload()
    );
    for _ in 0..2 {
        preview.emit_by_name::<()>("activate", &[]);
        wait_until("skill preview shown", || manual.is_mapped()).await;
        glib::timeout_future(Duration::from_millis(100)).await;
        save_theme_snapshot(dialog.upcast_ref(), "skills-preview");
        preview.emit_by_name::<()>("activate", &[]);
        wait_until("skill preview collapsed", || !manual.is_mapped()).await;
    }
    let home = agent::resolved_home().unwrap();
    let overrides = SkillOverrides::from_env();
    for &target in Skill::Team.targets() {
        let button: gtk::Button = theme_widget(
            dialog.upcast_ref(),
            &format!("flowmux-team-install-{}", target.slug()),
        );
        wait_until("team install ready", || {
            button.label().as_deref() == Some("Install")
        })
        .await;
        let path = Skill::Team.path(target, &home, None, &overrides);
        assert!(!path.exists());
        button.emit_clicked();
        wait_until("team bundle installed", || {
            button.label().as_deref() == Some("Up to date")
        })
        .await;
        assert_eq!(Skill::Team.doctor(&path), agent::DoctorStatus::Ok);
        let root = path.parent().unwrap();
        std::fs::write(root.join("scripts/team.py"), "custom helper").unwrap();
        std::fs::remove_file(root.join("references/sample.md")).unwrap();
        refresh.emit_clicked();
        wait_until("team repair offered", || {
            button.label().as_deref() == Some("Update")
        })
        .await;
        button.emit_clicked();
        wait_until("team repaired", || {
            button.label().as_deref() == Some("Up to date")
        })
        .await;
        assert_eq!(Skill::Team.doctor(&path), agent::DoctorStatus::Ok);
        let backups: Vec<_> = std::fs::read_dir(root.join("scripts"))
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".flowmux-backup-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            "custom helper"
        );
        let remove: gtk::Button = theme_widget(
            dialog.upcast_ref(),
            &format!("flowmux-team-remove-{}", target.slug()),
        );
        remove.emit_clicked();
        wait_until("team removed", || {
            button.label().as_deref() == Some("Install")
        })
        .await;
        assert!(!Skill::Team.is_present(&path));
        assert!(backups[0].path().exists());
    }
    let mut paths = Vec::new();
    for &target in Target::ALL {
        let button: gtk::Button = theme_widget(
            dialog.upcast_ref(),
            &format!("flowmux-skill-install-{}", target.slug()),
        );
        wait_until("skill install button ready", || {
            button.label().as_deref() == Some("Install") && button.is_mapped()
        })
        .await;
        let remove: gtk::Button = theme_widget(
            dialog.upcast_ref(),
            &format!("flowmux-skill-remove-{}", target.slug()),
        );
        assert!(!remove.is_sensitive(), "missing skills cannot be removed");
        let path = overrides.path(target, &home, None);
        assert!(
            !path.exists(),
            "fresh skill check must not install anything"
        );
        button.emit_clicked();
        button.emit_clicked(); // in-flight guard also protects programmatic repeat activation
        wait_until("skill installed", || {
            button.label().as_deref() == Some("Up to date")
        })
        .await;
        assert!(button.is_sensitive());
        assert!(remove.is_sensitive());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), Target::payload());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        paths.push((target, path, button));
    }
    glib::timeout_future(Duration::from_millis(100)).await;
    save_theme_snapshot(dialog.upcast_ref(), "skills-installed");
    let codex_row: adw::ActionRow = theme_widget(dialog.upcast_ref(), "flowmux-skill-codex");
    assert!(codex_row
        .subtitle()
        .unwrap()
        .contains("Another copy remains"));
    // Matching linked directories are healthy but intentionally not removable.
    let (_, linked_dir_path, _) = paths
        .iter()
        .find(|(target, _, _)| *target == Target::Antigravity)
        .unwrap();
    let directory_source = home.join("managed-dotfiles");
    std::fs::rename(linked_dir_path.parent().unwrap(), &directory_source).unwrap();
    std::os::unix::fs::symlink(&directory_source, linked_dir_path.parent().unwrap()).unwrap();
    refresh.emit_clicked();
    let linked_dir_row: adw::ActionRow =
        theme_widget(dialog.upcast_ref(), "flowmux-skill-antigravity");
    let linked_dir_remove: gtk::Button =
        theme_widget(dialog.upcast_ref(), "flowmux-skill-remove-antigravity");
    wait_until("linked directory explanation", || {
        linked_dir_row
            .subtitle()
            .is_some_and(|s| s.contains("Managed through a linked folder"))
    })
    .await;
    assert!(!linked_dir_remove.is_sensitive());
    assert_eq!(
        std::fs::read_to_string(directory_source.join("SKILL.md")).unwrap(),
        Target::payload()
    );
    std::fs::remove_file(linked_dir_path.parent().unwrap()).unwrap();
    std::fs::rename(&directory_source, linked_dir_path.parent().unwrap()).unwrap();

    // An external edit is exposed as Update; replacement keeps exactly one backup.
    let (_, path, button) = paths
        .iter()
        .find(|(target, _, _)| *target == Target::Codex)
        .unwrap();
    std::fs::write(path, "custom skill notes").unwrap();
    button.emit_clicked();
    wait_until("skill update offered", || {
        button.label().as_deref() == Some("Update")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "custom skill notes",
        "checking status must not overwrite external edits"
    );
    button.emit_clicked();
    button.emit_clicked();
    wait_until("skill updated", || {
        button.label().as_deref() == Some("Up to date")
    })
    .await;
    let backups = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("SKILL.md.flowmux-backup-")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&backups[0]).unwrap(),
        "custom skill notes"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), Target::payload());

    // Protect user-managed links and show the reason in the row.
    let (_, linked, linked_button) = paths
        .iter()
        .find(|(target, _, _)| *target == Target::ClaudeCode)
        .unwrap();
    std::fs::remove_file(linked).unwrap();
    let source = home.join("custom-skill.md");
    std::fs::write(&source, "user-managed").unwrap();
    std::os::unix::fs::symlink(&source, linked).unwrap();
    refresh.emit_clicked();
    wait_until("linked skill refused", || {
        linked_button.label().as_deref() == Some("Unavailable")
    })
    .await;
    let linked_row: adw::ActionRow = theme_widget(dialog.upcast_ref(), "flowmux-skill-claude-code");
    assert!(linked_row.subtitle().unwrap().contains("symlink"));
    assert!(!linked_button.is_sensitive());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "user-managed");

    // A new file appearing after the status check needs a separate Update click.
    let (_, raced, raced_button) = paths
        .iter()
        .find(|(target, _, _)| *target == Target::OpenCode)
        .unwrap();
    std::fs::remove_file(raced).unwrap();
    refresh.emit_clicked();
    wait_until("missing skill rechecked", || {
        raced_button.label().as_deref() == Some("Install")
    })
    .await;
    std::fs::write(raced, "created after check").unwrap();
    raced_button.emit_clicked();
    wait_until("stale install refused", || {
        raced_button.label().as_deref() == Some("Update")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(raced).unwrap(),
        "created after check"
    );
    let raced_row: adw::ActionRow = theme_widget(dialog.upcast_ref(), "flowmux-skill-opencode");
    assert!(raced_row
        .subtitle()
        .unwrap()
        .starts_with("Installation failed:"));

    // A stale Remove button must surface filesystem errors without deleting data.
    let (_, cline_path, _) = paths
        .iter()
        .find(|(target, _, _)| *target == Target::Cline)
        .unwrap();
    let cline_remove: gtk::Button = theme_widget(dialog.upcast_ref(), "flowmux-skill-remove-cline");
    wait_until("remove ready after refresh", || cline_remove.is_sensitive()).await;
    std::fs::remove_file(cline_path).unwrap();
    std::fs::create_dir(cline_path).unwrap();
    cline_remove.emit_clicked();
    let cline_row: adw::ActionRow = theme_widget(dialog.upcast_ref(), "flowmux-skill-cline");
    wait_until("removal failure shown", || {
        cline_row
            .subtitle()
            .is_some_and(|s| s.starts_with("Removal failed:"))
    })
    .await;
    assert!(cline_path.is_dir());
    assert!(!cline_remove.is_sensitive());
    std::fs::remove_dir(cline_path).unwrap();
    std::fs::write(cline_path, Target::payload()).unwrap();
    refresh.emit_clicked();

    // Removal is scoped to the selected skill, preserves custom bytes and
    // supporting files, and never removes agent settings or wrapper scripts.
    let settings = home.join(".claude/settings.json");
    std::fs::write(&settings, "{\"hooks\":{}}").unwrap();
    let wrapper = home.join("agent-wrapper");
    std::fs::write(&wrapper, "keep wrapper").unwrap();
    let resource = path.parent().unwrap().join("notes.txt");
    std::fs::write(&resource, "keep supporting file").unwrap();
    for (index, (target, removed_path, install)) in paths.iter().enumerate() {
        let remove: gtk::Button = theme_widget(
            dialog.upcast_ref(),
            &format!("flowmux-skill-remove-{}", target.slug()),
        );
        wait_until("remove button ready", || {
            remove.is_sensitive() && remove.is_mapped()
        })
        .await;
        remove.emit_clicked();
        remove.emit_clicked();
        install.emit_clicked(); // in-flight removal also blocks a competing install
        wait_until("skill removed", || {
            install.label().as_deref() == Some("Install") && install.is_sensitive()
        })
        .await;
        assert!(!remove.is_sensitive());
        assert!(!removed_path.exists());
        assert!(removed_path.symlink_metadata().is_err());
        for (_, untouched, _) in &paths[index + 1..] {
            assert!(untouched.exists(), "removal must be agent-scoped");
        }
    }
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "user-managed");
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "unmanaged copy");
    assert!(codex_row
        .subtitle()
        .unwrap()
        .contains("Another copy remains"));
    assert!(codex_row.subtitle().unwrap().contains("after removal here"));
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        "{\"hooks\":{}}"
    );
    assert_eq!(std::fs::read_to_string(&wrapper).unwrap(), "keep wrapper");
    assert_eq!(
        std::fs::read_to_string(&resource).unwrap(),
        "keep supporting file"
    );
    assert_eq!(
        std::fs::read_to_string(&backups[0]).unwrap(),
        "custom skill notes"
    );
    let removed_backups = std::fs::read_dir(raced.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(removed_backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&removed_backups[0]).unwrap(),
        "created after check"
    );
    let removed_row: adw::ActionRow = theme_widget(dialog.upcast_ref(), "flowmux-skill-opencode");
    assert!(removed_row
        .subtitle()
        .unwrap()
        .starts_with("Removed · Modified content saved to"));

    glib::timeout_future(Duration::from_millis(100)).await;
    save_theme_snapshot(dialog.upcast_ref(), "skills-removed");
    dialog.close();
    controller.dispatch(GtkCommand::ShowOptionsDialog).await;
    let reopened = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.widget_name() == "flowmux-options-dialog" && w.is_visible())
        .unwrap();
    let installed: gtk::Button = theme_widget(reopened.upcast_ref(), "flowmux-skill-install-codex");
    let reopened_stack: adw::ViewStack =
        theme_widget(reopened.upcast_ref(), "flowmux-options-stack");
    reopened_stack.set_visible_child_name("skills");
    wait_until("reopened removed skill status", || {
        installed.label().as_deref() == Some("Install") && installed.is_mapped()
    })
    .await;
    installed.emit_clicked();
    wait_until("removed skill reinstalled", || {
        installed.label().as_deref() == Some("Up to date")
    })
    .await;
    assert_eq!(std::fs::read_to_string(path).unwrap(), Target::payload());
    assert!(installed.is_sensitive());
    println!("MACOS_NATIVE_SKILLS_REMOVE_REINSTALL_OK");
    reopened.close();
    println!("MACOS_NATIVE_SKILLS_INSTALL_UPDATE_OK");
}
