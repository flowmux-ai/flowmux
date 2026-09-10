// SPDX-License-Identifier: GPL-3.0-or-later
//! On-demand search of retained VTE output; no transcript database or agent hooks.

use super::*;
use vte::prelude::*;

const RESULT_LIMIT: usize = 500;
const CAPTURE_ROWS: i64 = 512;

#[derive(Debug)]
struct OutputSnapshot {
    first: i64,
    last: i64,
    columns: i64,
    text: String,
}

#[derive(Clone, Debug)]
struct LineMatch {
    start: usize,
    end: usize,
    column: usize,
    needle: String,
}

#[derive(Clone)]
struct SearchHit {
    surface: SurfaceId,
    terminal: glib::WeakRef<vte::Terminal>,
    snapshot: Arc<OutputSnapshot>,
    line: LineMatch,
}

struct SearchUi {
    window: glib::WeakRef<gtk::Window>,
    entry: glib::WeakRef<gtk::SearchEntry>,
    case_toggle: glib::WeakRef<gtk::CheckButton>,
    list: glib::WeakRef<gtk::ListBox>,
    status: glib::WeakRef<gtk::Label>,
    more: glib::WeakRef<gtk::Button>,
    limit: Cell<usize>,
    generation: Cell<u64>,
    hits: RefCell<Vec<SearchHit>>,
}

fn matching_lines(
    text: &str,
    query: &str,
    match_case: bool,
    limit: usize,
) -> (Vec<LineMatch>, usize) {
    let query = if match_case {
        query.to_owned()
    } else {
        query.to_lowercase()
    };
    if query.is_empty() {
        return (Vec::new(), 0);
    }
    let mut matches = Vec::new();
    let mut count = 0;
    let mut offset = 0;
    for part in text.split_inclusive('\n') {
        let line = part.strip_suffix('\n').unwrap_or(part);
        let found = if match_case {
            line.contains(&query)
        } else {
            line.to_lowercase().contains(&query)
        };
        if found {
            count += 1;
            if matches.len() < limit {
                let column = match_column(line, &query, match_case);
                matches.push(LineMatch {
                    start: offset,
                    end: offset + line.len(),
                    column,
                    // Bound the native regex even for megabyte JSON/log lines.
                    needle: line
                        .chars()
                        .skip(column)
                        .take(query.chars().count().min(256))
                        .collect(),
                });
            }
        }
        offset += part.len();
    }
    (matches, count)
}

fn match_column(line: &str, query: &str, match_case: bool) -> usize {
    let folded = if match_case {
        line.to_owned()
    } else {
        line.to_lowercase()
    };
    let query = if match_case {
        query.to_owned()
    } else {
        query.to_lowercase()
    };
    let offset = folded.find(&query).unwrap_or(0);
    let mut bytes = 0;
    line.chars()
        .take_while(|ch| {
            bytes += if match_case {
                ch.len_utf8()
            } else {
                ch.to_lowercase().map(char::len_utf8).sum()
            };
            bytes <= offset
        })
        .count()
}

fn match_preview(line: &str, column: usize) -> String {
    let start = column.saturating_sub(40);
    let preview: String = line.chars().skip(start).take(180).collect();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        preview,
        if line.chars().count() > start + 180 {
            "…"
        } else {
            ""
        }
    )
}

impl SearchUi {
    fn current(&self, generation: u64) -> bool {
        self.generation.get() == generation && self.window.upgrade().is_some_and(|w| w.is_visible())
    }

    fn schedule(self: &Rc<Self>, controller: &WindowController) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.hits.borrow_mut().clear();
        if let Some(more) = self.more.upgrade() {
            more.set_visible(false);
        }
        if let Some(list) = self.list.upgrade() {
            list.remove_all();
        }
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        let query = entry.text().to_string();
        if let Some(status) = self.status.upgrade() {
            status.set_text(if query.is_empty() {
                "Search retained output in all workspaces in this window"
            } else {
                "Searching…"
            });
        }
        if query.is_empty() {
            return;
        }
        let ui = self.clone();
        let controller = controller.clone();
        glib::MainContext::default().spawn_local(async move {
            glib::timeout_future(Duration::from_millis(180)).await;
            if ui.current(generation) {
                controller
                    .search_terminal_output(&ui, generation, query)
                    .await;
            }
        });
    }
}

impl WindowController {
    pub(super) fn show_terminal_output_search(&self) {
        // Repeated shortcut presses focus the existing search for this window.
        for window in gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
        {
            if window.widget_name() == "terminal-output-search"
                && window.transient_for().as_ref() == Some(self.window.upcast_ref())
            {
                window.present();
                return;
            }
        }
        let dialog = gtk::Window::builder()
            .title("Search all terminals")
            .transient_for(&self.window)
            .modal(true)
            .default_width(760)
            .default_height(520)
            .build();
        dialog.set_widget_name("terminal-output-search");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Search terminal output…")
            .hexpand(true)
            .build();
        entry.set_widget_name("terminal-output-query");
        let case_toggle = gtk::CheckButton::with_label("Match case");
        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.set_tooltip_text(Some("Search current output again"));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&entry);
        row.append(&case_toggle);
        row.append(&refresh);
        content.append(&row);
        let status = gtk::Label::builder()
            .label("Search retained output in all workspaces in this window")
            .xalign(0.0)
            .wrap(true)
            .build();
        status.add_css_class("dim-label");
        content.append(&status);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.add_css_class("boxed-list");
        content.append(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        let more = gtk::Button::with_label("Show more results");
        more.set_visible(false);
        content.append(&more);
        dialog.set_child(Some(&content));
        let ui = Rc::new(SearchUi {
            window: dialog.downgrade(),
            entry: entry.downgrade(),
            case_toggle: case_toggle.downgrade(),
            list: list.downgrade(),
            status: status.downgrade(),
            more: more.downgrade(),
            limit: Cell::new(RESULT_LIMIT),
            generation: Cell::new(0),
            hits: RefCell::new(Vec::new()),
        });
        entry.connect_changed({
            let ui = ui.clone();
            let controller = self.clone();
            move |_| {
                ui.limit.set(RESULT_LIMIT);
                ui.schedule(&controller);
            }
        });
        case_toggle.connect_toggled({
            let ui = ui.clone();
            let controller = self.clone();
            move |_| ui.schedule(&controller)
        });
        refresh.connect_clicked({
            let ui = ui.clone();
            let controller = self.clone();
            move |_| ui.schedule(&controller)
        });
        more.connect_clicked({
            let ui = ui.clone();
            let controller = self.clone();
            move |_| {
                ui.limit.set(ui.limit.get().saturating_add(RESULT_LIMIT));
                ui.schedule(&controller);
            }
        });
        entry.connect_activate({
            let list = list.downgrade();
            move |_| {
                if let Some(list) = list.upgrade() {
                    if let Some(row) = list.selected_row().or_else(|| list.row_at_index(0)) {
                        row.activate();
                    }
                }
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let ui = ui.clone();
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Down {
                    if let Some(list) = ui.list.upgrade() {
                        if let Some(row) = list.row_at_index(0) {
                            list.select_row(Some(&row));
                            row.grab_focus();
                        }
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        entry.add_controller(keys);
        let close_key = gtk::EventControllerKey::new();
        close_key.set_propagation_phase(gtk::PropagationPhase::Capture);
        close_key.connect_key_pressed({
            let dialog = dialog.downgrade();
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        dialog.add_controller(close_key);
        list.connect_row_activated({
            let ui = ui.clone();
            let controller = self.clone();
            move |_, row| {
                let Some(hit) = ui.hits.borrow().get(row.index() as usize).cloned() else {
                    return;
                };
                let generation = ui.generation.get().wrapping_add(1);
                ui.generation.set(generation);
                let ui = ui.clone();
                let controller = controller.clone();
                glib::MainContext::default().spawn_local(async move {
                    let result = controller
                        .open_terminal_search_hit(&hit, || ui.current(generation))
                        .await;
                    if !ui.current(generation) {
                        return;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(window) = ui.window.upgrade() {
                                window.close();
                            }
                        }
                        Err(message) => {
                            if let Some(status) = ui.status.upgrade() {
                                status.set_text(message);
                            }
                        }
                    }
                });
            }
        });
        dialog.connect_close_request({
            let ui = ui.clone();
            move |_| {
                ui.generation.set(ui.generation.get().wrapping_add(1));
                glib::Propagation::Proceed
            }
        });
        dialog.present();
        entry.grab_focus();
    }

    async fn search_terminal_output(&self, ui: &Rc<SearchUi>, generation: u64, query: String) {
        let match_case = ui.case_toggle.upgrade().is_some_and(|b| b.is_active());
        let workspaces = self.store.ordered_workspaces().await;
        let mut sources = Vec::new();
        for workspace in workspaces {
            for root in &workspace.surfaces {
                root.root_pane.for_each_leaf(|pane| {
                    if let Some(PaneContent::Tabs { surfaces, .. }) =
                        root.root_pane.find_leaf_content(pane)
                    {
                        for surface in surfaces {
                            if self
                                .pane_registry
                                .borrow()
                                .terminals
                                .contains_key(&surface.id)
                            {
                                sources.push((
                                    surface.id,
                                    format!("{} / {}", workspace.display_title(), surface.title),
                                ));
                            }
                        }
                    }
                });
            }
        }
        let mut count = 0;
        let mut searched = 0;
        let mut unavailable = 0;
        'sources: for (surface, title) in sources {
            if !ui.current(generation) {
                return;
            }
            let terminal = self.pane_registry.borrow().terminals.get(&surface).cloned();
            let Some(terminal) = terminal else {
                continue;
            };
            let (first, last) = terminal.output_search_range();
            let columns = terminal.widget.column_count();
            let mut text = String::new();
            for start in (first..last).step_by(CAPTURE_ROWS as usize) {
                if !ui.current(generation) {
                    return;
                }
                let Some(chunk) =
                    terminal.output_search_text(start, (start + CAPTURE_ROWS).min(last))
                else {
                    unavailable += 1;
                    continue 'sources;
                };
                text.push_str(&chunk);
                glib::timeout_future(Duration::from_millis(1)).await;
            }
            if terminal.widget.column_count() != columns {
                unavailable += 1;
                continue;
            }
            let snapshot = Arc::new(OutputSnapshot {
                first,
                last,
                columns,
                text,
            });
            let remaining = ui.limit.get().saturating_sub(ui.hits.borrow().len());
            let (worker_snapshot, worker_query) = (snapshot.clone(), query.clone());
            // ponytail: snapshot each bounded VTE buffer on demand; index incrementally
            // only if measured extraction latency warrants the extra retained state.
            let Ok((matches, total)) = gtk::gio::spawn_blocking(move || {
                matching_lines(&worker_snapshot.text, &worker_query, match_case, remaining)
            })
            .await
            else {
                unavailable += 1;
                continue;
            };
            if !ui.current(generation) {
                return;
            }
            searched += 1;
            count += total;
            let Some(list) = ui.list.upgrade() else {
                return;
            };
            for line in matches {
                let labels = gtk::Box::new(gtk::Orientation::Vertical, 4);
                labels.set_margin_top(8);
                labels.set_margin_bottom(8);
                labels.set_margin_start(10);
                labels.set_margin_end(10);
                let location = gtk::Label::builder()
                    .label(&title)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .build();
                location.add_css_class("dim-label");
                labels.append(&location);
                let text = &snapshot.text[line.start..line.end];
                let preview = gtk::Label::builder()
                    .label(match_preview(text, line.column))
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build();
                preview.add_css_class("monospace");
                labels.append(&preview);
                ui.hits.borrow_mut().push(SearchHit {
                    surface,
                    terminal: terminal.widget.downgrade(),
                    snapshot: snapshot.clone(),
                    line,
                });
                list.append(&labels);
            }
            if let Some(status) = ui.status.upgrade() {
                status.set_text(&format!(
                    "Searching… {count} matching lines in {searched} terminals"
                ));
            }
            glib::timeout_future(Duration::from_millis(1)).await;
        }
        if !ui.current(generation) {
            return;
        }
        if let Some(status) = ui.status.upgrade() {
            let shown = ui.hits.borrow().len();
            let mut message = if count > shown {
                format!("Showing {shown} of {count} matching lines across {searched} terminals")
            } else {
                format!("{count} matching lines across {searched} terminals · Refresh to include new output")
            };
            if unavailable > 0 {
                message.push_str(&format!(
                    " · {unavailable} terminals changed or unavailable; refresh to retry"
                ));
            }
            status.set_text(&message);
        }
        if let Some(more) = ui.more.upgrade() {
            more.set_visible(count > ui.hits.borrow().len());
        }
        if let Some(list) = ui.list.upgrade() {
            list.select_row(list.row_at_index(0).as_ref());
        }
    }

    async fn open_terminal_search_hit(
        &self,
        hit: &SearchHit,
        current: impl Fn() -> bool,
    ) -> Result<(), &'static str> {
        let terminal = {
            let registry = self.pane_registry.borrow();
            let terminal = registry
                .terminals
                .get(&hit.surface)
                .ok_or("This tab has closed. Refresh the search.")?;
            if hit.terminal.upgrade().as_ref() != Some(&terminal.widget) {
                return Err("This terminal was replaced. Refresh the search.");
            }
            terminal.clone()
        };
        // A prefix check permits appended output, but refuses rebased/evicted or
        // rewritten matches instead of selecting another identical-looking line.
        if terminal.widget.column_count() != hit.snapshot.columns {
            return Err("This output has changed or left scrollback. Refresh the search.");
        }
        let prefix_end = hit.line.end
            + usize::from(hit.snapshot.text.as_bytes().get(hit.line.end) == Some(&b'\n'));
        let mut checked = 0;
        for start in (hit.snapshot.first..hit.snapshot.last).step_by(CAPTURE_ROWS as usize) {
            if !current() {
                return Err("Search cancelled.");
            }
            let chunk = terminal
                .output_search_text(start, (start + CAPTURE_ROWS).min(hit.snapshot.last))
                .ok_or("Output is unavailable. Refresh the search.")?;
            let remaining = &hit.snapshot.text.as_bytes()[checked..prefix_end];
            let length = remaining.len().min(chunk.len());
            if chunk.as_bytes()[..length] != remaining[..length] {
                return Err("This output has changed or left scrollback. Refresh the search.");
            }
            checked += length;
            if checked == prefix_end {
                break;
            }
            glib::timeout_future(Duration::from_millis(1)).await;
        }
        if checked != prefix_end {
            return Err("This output has left scrollback. Refresh the search.");
        }
        let (workspace, pane) = {
            let registry = self.pane_registry.borrow();
            if registry.terminals.get(&hit.surface).map(|t| &t.widget) != Some(&terminal.widget)
                || terminal.widget.column_count() != hit.snapshot.columns
            {
                return Err("This terminal has changed. Refresh the search.");
            }
            (
                *registry
                    .surface_workspace
                    .get(&hit.surface)
                    .ok_or("This workspace has closed.")?,
                registry
                    .pane_for_surface(hit.surface)
                    .ok_or("This tab has moved. Refresh the search.")?,
            )
        };
        if !current() {
            return Err("Search cancelled.");
        }
        let occurrence = hit.snapshot.text[..hit.line.start]
            .split_inclusive('\n')
            .filter(|line| {
                line.char_indices()
                    .nth(hit.line.column)
                    .is_some_and(|(offset, _)| line[offset..].starts_with(&hit.line.needle))
            })
            .count();
        self.activate_workspace(workspace).await;
        self.activate_surface_now(pane, hit.surface).await;
        self.focus_pane(pane);
        if !terminal.find_output_match(&hit.line.needle, hit.line.column, occurrence) {
            return Err("This output is no longer searchable. Refresh the search.");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descendants<T: IsA<gtk::Widget> + glib::object::IsClass>(
        root: &impl IsA<gtk::Widget>,
    ) -> Vec<T> {
        let mut widgets = vec![root.clone().upcast::<gtk::Widget>()];
        let mut found = Vec::new();
        while let Some(widget) = widgets.pop() {
            if let Ok(value) = widget.clone().downcast::<T>() {
                found.push(value);
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                child = next.next_sibling();
                widgets.push(next);
            }
        }
        found
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    async fn output_search_finds_hidden_tabs_and_navigates_to_retained_wrapped_output() {
        let (controller, foreground, _) = super::super::tests::build_single_workspace_controller(
            "com.flowmux.App.UiTest.OutputSearch",
        )
        .await;
        let background = controller
            .store
            .create_workspace(Some("Background".into()), std::env::temp_dir())
            .await;
        let workspace = controller.store.get_workspace(background).await.unwrap();
        let pane = workspace.surfaces[0].root_pane.first_leaf_id().unwrap();
        let target = workspace.surfaces[0]
            .root_pane
            .active_surface_id(pane)
            .unwrap();
        controller
            .store
            .rename_surface(pane, target, "Hidden output".into())
            .await;
        controller
            .store
            .add_terminal_surface_to_pane(pane, None)
            .await
            .unwrap();
        let workspace = controller.store.get_workspace(background).await.unwrap();
        controller.render_workspace(&workspace);
        controller.window.present();
        controller.activate_workspace(background).await;
        glib::timeout_future(Duration::from_millis(100)).await;
        let terminal = controller.pane_registry.borrow().terminals[&target].clone();
        terminal.widget.set_scrollback_lines(1000);
        terminal.widget.reset(true, true);
        let long = format!("{} NEEDLE 한글 [x]", "wrapped-".repeat(80));
        let output = format!(
            "first NEEDLE\r\n{long}\r\n{}",
            (0..150)
                .map(|n| format!("filler {n}\r\n"))
                .collect::<String>()
        );
        terminal.widget.feed(output.as_bytes());
        glib::timeout_future(Duration::from_millis(100)).await;
        controller.activate_workspace(foreground).await;
        controller.show_terminal_output_search();
        let dialog = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.widget_name() == "terminal-output-search")
            .unwrap();
        let entry = descendants::<gtk::SearchEntry>(&dialog).pop().unwrap();
        let list = descendants::<gtk::ListBox>(&dialog).pop().unwrap();
        entry.set_text("needle");
        entry.set_text("no-such-output");
        glib::timeout_future(Duration::from_millis(250)).await;
        assert!(
            list.row_at_index(0).is_none(),
            "superseded searches must not publish results"
        );
        entry.set_text("needle");
        for _ in 0..100 {
            if list.row_at_index(1).is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        assert_eq!(descendants::<gtk::ListBoxRow>(&list).len(), 2);
        assert_eq!(
            controller.store.snapshot().await.active_workspace,
            Some(foreground)
        );
        assert!(
            !terminal.widget.is_mapped(),
            "search must not activate a hidden tab"
        );
        let row = list.row_at_index(1).unwrap();
        assert!(descendants::<gtk::Label>(&row)
            .iter()
            .any(|l| l.text().contains("NEEDLE 한글")));
        list.emit_by_name::<()>("row-activated", &[&row]);
        for _ in 0..100 {
            if !dialog.is_visible() {
                break;
            }
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        assert!(
            !dialog.is_visible(),
            "navigation failed: {:?}",
            descendants::<gtk::Label>(&dialog)
                .iter()
                .map(|l| l.text())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            controller.store.snapshot().await.active_workspace,
            Some(background)
        );
        assert_eq!(
            controller.pane_registry.borrow().active_surface(pane),
            Some(target)
        );
        assert_eq!(
            terminal.widget.text_selected(vte::Format::Text).as_deref(),
            Some("NEEDLE")
        );
        let adjustment = terminal.widget.vadjustment().unwrap();
        assert!(
            adjustment.value() < adjustment.upper() - adjustment.page_size(),
            "result must scroll to retained output"
        );

        terminal.widget.feed(
            (0..510)
                .map(|n| format!("MORE_ITEM {n}\r\n"))
                .collect::<String>()
                .as_bytes(),
        );
        glib::timeout_future(Duration::from_millis(100)).await;
        controller.show_terminal_output_search();
        let expanded_dialog = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.widget_name() == "terminal-output-search" && w.is_visible())
            .unwrap();
        let expanded_entry = descendants::<gtk::SearchEntry>(&expanded_dialog)
            .pop()
            .unwrap();
        let expanded_list = descendants::<gtk::ListBox>(&expanded_dialog).pop().unwrap();
        let more = descendants::<gtk::Button>(&expanded_dialog)
            .into_iter()
            .find(|b| b.label().as_deref() == Some("Show more results"))
            .unwrap();
        expanded_entry.set_text("MORE_ITEM");
        for _ in 0..100 {
            if more.is_visible() {
                break;
            }
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        assert_eq!(
            descendants::<gtk::ListBoxRow>(&expanded_list).len(),
            RESULT_LIMIT
        );
        assert!(more.is_visible());
        more.emit_clicked();
        for _ in 0..100 {
            if expanded_list.row_at_index(509).is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        assert_eq!(descendants::<gtk::ListBoxRow>(&expanded_list).len(), 510);
        expanded_dialog.close();

        // A result near the end requires multiple yielding validation chunks.
        terminal.widget.set_scrollback_lines(10_000);
        terminal
            .widget
            .feed(format!("{}CANCEL_TARGET\r\n", "padding\r\n".repeat(4000)).as_bytes());
        glib::timeout_future(Duration::from_millis(100)).await;
        controller.activate_workspace(foreground).await;
        controller.show_terminal_output_search();
        let cancel_dialog = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.widget_name() == "terminal-output-search" && w.is_visible())
            .unwrap();
        let cancel_entry = descendants::<gtk::SearchEntry>(&cancel_dialog)
            .pop()
            .unwrap();
        let cancel_list = descendants::<gtk::ListBox>(&cancel_dialog).pop().unwrap();
        cancel_entry.set_text("CANCEL_TARGET");
        for _ in 0..100 {
            if cancel_list.row_at_index(0).is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        cancel_list.emit_by_name::<()>("row-activated", &[&cancel_list.row_at_index(0).unwrap()]);
        glib::timeout_future(Duration::from_millis(2)).await;
        cancel_dialog.close();
        glib::timeout_future(Duration::from_millis(150)).await;
        assert_eq!(
            controller.store.snapshot().await.active_workspace,
            Some(foreground),
            "closing search must cancel navigation already validating output"
        );

        controller.show_terminal_output_search();
        let query_dialog = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.widget_name() == "terminal-output-search" && w.is_visible())
            .unwrap();
        let query_entry = descendants::<gtk::SearchEntry>(&query_dialog)
            .pop()
            .unwrap();
        let query_list = descendants::<gtk::ListBox>(&query_dialog).pop().unwrap();
        query_entry.set_text("CANCEL_TARGET");
        for _ in 0..100 {
            if query_list.row_at_index(0).is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(25)).await;
        }
        query_list.emit_by_name::<()>("row-activated", &[&query_list.row_at_index(0).unwrap()]);
        glib::timeout_future(Duration::from_millis(2)).await;
        query_entry.set_text("no-longer-the-same-query");
        glib::timeout_future(Duration::from_millis(300)).await;
        assert_eq!(
            controller.store.snapshot().await.active_workspace,
            Some(foreground)
        );
        assert!(query_dialog.is_visible());
        assert!(query_list.row_at_index(0).is_none());
        query_dialog.close();

        let (first, last) = terminal.output_search_range();
        let snapshot = Arc::new(OutputSnapshot {
            first,
            last,
            columns: terminal.widget.column_count(),
            text: terminal.output_search_text(first, last).unwrap(),
        });
        let line = matching_lines(&snapshot.text, "NEEDLE", true, 10)
            .0
            .remove(0);
        let hit = SearchHit {
            surface: target,
            terminal: terminal.widget.downgrade(),
            snapshot,
            line,
        };
        terminal.widget.feed(b"appended after search\r\n");
        glib::timeout_future(Duration::from_millis(50)).await;
        assert!(
            controller
                .open_terminal_search_hit(&hit, || true)
                .await
                .is_ok(),
            "appending output must preserve existing results"
        );
        terminal
            .widget
            .set_size(hit.snapshot.columns + 1, terminal.widget.row_count());
        assert!(
            controller
                .open_terminal_search_hit(&hit, || true)
                .await
                .is_err(),
            "resized output must reject stale row coordinates"
        );
        terminal
            .widget
            .set_size(hit.snapshot.columns, terminal.widget.row_count());
        controller
            .move_surface_to_workspace(pane, target, foreground)
            .await
            .unwrap();
        assert!(
            controller
                .open_terminal_search_hit(&hit, || true)
                .await
                .is_ok(),
            "a moved tab must resolve its current workspace and pane"
        );
        assert_eq!(
            controller.store.snapshot().await.active_workspace,
            Some(foreground)
        );
        terminal.widget.reset(true, true);
        terminal.widget.feed(b"replacement NEEDLE\r\n");
        glib::timeout_future(Duration::from_millis(50)).await;
        assert!(
            controller
                .open_terminal_search_hit(&hit, || true)
                .await
                .is_err(),
            "rewritten output must not jump to a different match"
        );
        controller
            .pane_registry
            .borrow_mut()
            .terminals
            .remove(&target);
        assert_eq!(
            controller
                .open_terminal_search_hit(&hit, || true)
                .await
                .unwrap_err(),
            "This tab has closed. Refresh the search."
        );
        terminal.close_pty();
        for terminal in controller.pane_registry.borrow().terminals.values() {
            terminal.close_pty();
        }
        controller.window.close();
    }

    #[test]
    fn output_search_matches_unicode_literals_duplicates_and_counts_beyond_display_limit() {
        let text = "prefix\n한글 Error [x]\n한글 Error [x]\nerror [X]\nno match\n";
        let (hits, total) = matching_lines(text, "error [x]", false, 2);
        assert_eq!(total, 3);
        assert_eq!(hits.len(), 2);
        assert_eq!(&text[hits[1].start..hits[1].end], "한글 Error [x]");
        assert_eq!(hits[1].column, 3);
        assert_eq!(hits[1].needle, "Error [x]");
        assert_eq!(matching_lines(text, "Error [x]", true, 10).1, 2);
        assert_eq!(matching_lines(text, "한글", true, 10).1, 2);
        assert_eq!(matching_lines(text, "", false, 10).1, 0);
        assert_eq!(match_column("İ 한글 ERROR", "error", false), 5);
        assert_eq!(match_column("😀 한글 ERROR", "한글", false), 2);
        assert_eq!(match_column("İ suffix", "\u{307}", false), 0);
        let line = format!("{} 한글 NEEDLE 끝", "가".repeat(300));
        let (hits, _) = matching_lines(&line, "needle", false, 1);
        assert!(match_preview(&line, hits[0].column).contains("한글 NEEDLE"));
    }

    #[gtk::test]
    async fn output_search_chunks_preserve_wraps_duplicates_and_alternate_screen() {
        let terminal = crate::ui::ghostty_pane::GhosttyPane::spawn(
            PaneId::new(),
            SurfaceId::new(),
            vec!["/bin/cat".into()],
            None,
            Vec::new(),
            1000,
            PaneCallbacks::noop_for_test(),
        );
        let window = gtk::Window::builder()
            .default_width(600)
            .default_height(300)
            .child(&terminal.container)
            .build();
        window.present();
        glib::timeout_future(Duration::from_millis(100)).await;
        let wide = format!(
            "{}BOUNDARY 한글",
            "x".repeat(terminal.widget.column_count() as usize + 10)
        );
        let output = format!(
            "{}{wide}\r\nDUPLICATE\r\n{}DUPLICATE\r\n",
            "filler\r\n".repeat(511),
            "middle\r\n".repeat(50)
        );
        terminal.widget.feed(output.as_bytes());
        glib::timeout_future(Duration::from_millis(100)).await;
        let (first, last) = terminal.output_search_range();
        let whole = terminal.output_search_text(first, last).unwrap();
        let chunks: String = (first..last)
            .step_by(CAPTURE_ROWS as usize)
            .map(|start| {
                terminal
                    .output_search_text(start, (start + CAPTURE_ROWS).min(last))
                    .unwrap()
            })
            .collect();
        assert_eq!(whole, chunks);
        assert!(whole.contains(&wide));
        assert!(terminal.find_output_match("DUPLICATE", 0, 0));
        glib::timeout_future(Duration::from_millis(50)).await;
        let first_position = terminal.widget.vadjustment().unwrap().value();
        assert!(terminal.find_output_match("DUPLICATE", 0, 1));
        glib::timeout_future(Duration::from_millis(50)).await;
        assert!(terminal.widget.vadjustment().unwrap().value() > first_position);
        terminal.widget.set_scrollback_lines(100);
        terminal.widget.feed("tail\r\n".repeat(150).as_bytes());
        glib::timeout_future(Duration::from_millis(100)).await;
        let (first, last) = terminal.output_search_range();
        assert!(first > 0);
        let text = terminal.output_search_text(first, last).unwrap();
        assert!(text.contains("tail"));
        assert!(!text.contains("BOUNDARY"));
        terminal
            .widget
            .feed(b"\x1b[?1049h\x1b[H\x1b[2JALT_SEARCH\x1b[8;1HLOWER_SEARCH\x1b[H");
        terminal.set_alternate_screen(true);
        glib::timeout_future(Duration::from_millis(50)).await;
        let (first, last) = terminal.output_search_range();
        let text = terminal.output_search_text(first, last).unwrap();
        assert!(text.contains("ALT_SEARCH") && text.contains("LOWER_SEARCH"));
        assert!(!text.contains("tail"));
        assert!(terminal.find_output_match("LOWER_SEARCH", 0, 0));
        terminal.widget.feed(b"\x1b[?1049l");
        terminal.set_alternate_screen(false);
        terminal.widget.set_scrollback_lines(5000);
        terminal.widget.reset(true, true);
        let huge = format!("{} HUGE_NEEDLE", "abcdefgh".repeat(10_000));
        terminal.widget.feed(format!("{huge}\r\n").as_bytes());
        glib::timeout_future(Duration::from_millis(150)).await;
        let (first, last) = terminal.output_search_range();
        assert!(terminal
            .output_search_text(first, last)
            .unwrap()
            .contains(&huge));
        assert!(
            terminal.find_output_match("HUGE_NEEDLE", 80_001, 0),
            "large log lines must remain navigable"
        );
        assert_eq!(
            terminal.widget.text_selected(vte::Format::Text).as_deref(),
            Some("HUGE_NEEDLE")
        );
        terminal.widget.feed("😀 e\u{301} 한글 [x]\r\n".as_bytes());
        glib::timeout_future(Duration::from_millis(50)).await;
        assert!(terminal.find_output_match("[x]", 8, 0));
        assert_eq!(
            terminal.widget.text_selected(vte::Format::Text).as_deref(),
            Some("[x]")
        );
        terminal.close_pty();
        window.close();
    }
}
