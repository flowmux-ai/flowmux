// SPDX-License-Identifier: GPL-3.0-or-later

use comrak::arena_tree::NodeEdge;
use comrak::nodes::{AstNode, ListType, NodeTable, NodeValue, TableAlignment};
use comrak::{format_commonmark, parse_document, Arena, Options};
use gtk::prelude::*;

use super::{copy_button, label, text_label};

#[derive(Default)]
pub(super) struct Blocks(Vec<RenderedBlock>);

struct RenderedBlock {
    widget: gtk::Box,
    tree: Vec<Option<NodeValue>>,
}

impl Blocks {
    pub fn render(&mut self, parent: &gtk::Box, markdown: &str, live: bool) {
        let arena = Arena::new();
        let mut options = Options::default();
        options.extension.strikethrough = true;
        options.extension.autolink = true;
        options.extension.table = true;
        options.extension.tasklist = true;
        let document = parse_document(&arena, markdown, &options);
        let mut previous = None;
        let mut count = 0;
        for (index, node) in document.children().enumerate() {
            count = index + 1;
            let existing = self.0.get_mut(index);
            // Compare parsed blocks so a late reference definition can also update earlier text.
            let edges = || {
                node.traverse().map(|edge| match edge {
                    NodeEdge::Start(node) => Some(node),
                    NodeEdge::End(_) => None,
                })
            };
            if let Some(old) = existing.as_ref() {
                let mut nodes = edges();
                if old.tree.iter().all(|value| match (value, nodes.next()) {
                    (Some(value), Some(Some(node))) => *value == node.data().value,
                    (None, Some(None)) => true,
                    _ => false,
                }) && nodes.next().is_none()
                {
                    previous = Some(old.widget.clone());
                    continue;
                }
            }
            let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
            block(&widget, node, 0);
            let tree = if live {
                edges()
                    .map(|node| node.map(|n| n.data().value.clone()))
                    .collect()
            } else {
                Vec::new()
            };
            let rendered = RenderedBlock {
                widget: widget.clone(),
                tree,
            };
            if let Some(old) = existing {
                parent.remove(&old.widget);
                *old = rendered;
            } else {
                self.0.push(rendered);
            }
            parent.insert_child_after(&widget, previous.as_ref());
            previous = Some(widget);
        }
        for old in self.0.drain(count..) {
            parent.remove(&old.widget);
        }
        if !live {
            self.settle();
        }
    }

    pub fn settle(&mut self) {
        for block in &mut self.0 {
            block.tree = Vec::new();
        }
    }
}

fn escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

fn inline<'a>(node: &'a AstNode<'a>, depth: u16) -> String {
    if depth > 64 {
        return String::new();
    }
    let children = || {
        node.children()
            .map(|child| inline(child, depth + 1))
            .collect::<String>()
    };
    match &node.data().value {
        NodeValue::Text(text) => escape(text),
        NodeValue::SoftBreak => " ".into(),
        NodeValue::LineBreak => "\n".into(),
        NodeValue::Code(code) => format!("<tt>{}</tt>", escape(&code.literal)),
        NodeValue::Strong => format!("<b>{}</b>", children()),
        NodeValue::Emph => format!("<i>{}</i>", children()),
        NodeValue::Strikethrough => format!("<s>{}</s>", children()),
        NodeValue::HtmlInline(html) => escape(html),
        NodeValue::Link(link)
            if link.url.starts_with("https://") || link.url.starts_with("http://") =>
        {
            format!("<a href=\"{}\">{}</a>", escape(&link.url), children())
        }
        _ => children(),
    }
}

fn block<'a>(box_: &gtk::Box, node: &'a AstNode<'a>, depth: u16) {
    if depth > 32 {
        return;
    }
    match &node.data().value {
        NodeValue::CodeBlock(code_block) => {
            let language = code_block.info.split_whitespace().next().unwrap_or("");
            box_.append(&code(language, &code_block.literal));
        }
        NodeValue::HtmlBlock(html) => box_.append(&text_label(&html.literal)),
        NodeValue::Table(table) => render_table(box_, node, table),
        NodeValue::ThematicBreak => box_.append(&gtk::Separator::new(gtk::Orientation::Horizontal)),
        NodeValue::BlockQuote => {
            let quoted = gtk::Box::new(gtk::Orientation::Vertical, 4);
            quoted.set_margin_start(12);
            quoted.add_css_class("dim-label");
            for child in node.children() {
                block(&quoted, child, depth + 1);
            }
            box_.append(&quoted);
        }
        NodeValue::List(list) => {
            for (index, item) in node.children().enumerate() {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
                let marker =
                    match &item.data().value {
                        NodeValue::TaskItem(task) => {
                            let checked = task.symbol.is_some();
                            let marker = label(if checked { "☑" } else { "☐" });
                            marker.update_property(&[gtk::accessible::Property::Label(
                                if checked { "Checked" } else { "Unchecked" },
                            )]);
                            marker
                        }
                        _ if list.list_type == ListType::Ordered => {
                            label(&format!("{}.", list.start + index))
                        }
                        _ => label("•"),
                    };
                marker.set_valign(gtk::Align::Start);
                row.append(&marker);
                let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
                text.set_hexpand(true);
                for child in item.children() {
                    block(&text, child, depth + 1);
                }
                row.append(&text);
                box_.append(&row);
            }
        }
        _ => {
            let text = text_label("");
            let markup = inline(node, 0);
            match &node.data().value {
                NodeValue::Heading(heading) => {
                    text.set_markup(&format!("<b>{markup}</b>"));
                    if heading.level <= 2 {
                        text.add_css_class("title-4");
                    }
                }
                _ => text.set_markup(&markup),
            }
            box_.append(&text);
        }
    }
}

fn render_table<'a>(box_: &gtk::Box, node: &'a AstNode<'a>, table: &NodeTable) {
    // shortcut: show large tables as source to bound GTK widgets, revisit with virtualized rows.
    if table.num_rows.saturating_mul(table.num_columns) > 512 {
        let mut source = String::new();
        format_commonmark(node, &Options::default(), &mut source).unwrap();
        box_.append(&code("Markdown", &source));
        return;
    }
    let grid = gtk::Grid::new();
    grid.set_halign(gtk::Align::Start);
    grid.set_valign(gtk::Align::Start);
    grid.add_css_class("flowmux-chat-table");
    grid.update_property(&[gtk::accessible::Property::Label("Markdown table")]);
    for (y, row) in node.children().enumerate() {
        for (x, cell) in row.children().enumerate() {
            let text = text_label("");
            text.set_markup(&inline(cell, 0));
            text.set_width_chars(text.text().chars().take(12).count() as i32);
            text.set_xalign(match table.alignments[x] {
                TableAlignment::Center => 0.5,
                TableAlignment::Right => 1.0,
                _ => 0.0,
            });
            text.set_max_width_chars(28);
            text.add_css_class("flowmux-chat-table-cell");
            if y == 0 {
                text.add_css_class("heading");
            }
            grid.attach(&text, x as i32, y as i32, 1, 1);
        }
    }
    box_.append(
        &gtk::ScrolledWindow::builder()
            .child(&grid)
            .propagate_natural_height(true)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build(),
    );
}

pub(super) fn code(language: &str, code: &str) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 3);
    root.add_css_class("card");
    root.set_overflow(gtk::Overflow::Hidden);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toolbar.add_css_class("flowmux-chat-code-toolbar");
    let language = label(language);
    language.set_hexpand(true);
    language.add_css_class("dim-label");
    toolbar.append(&language);
    let copy = copy_button("Copy code");
    let raw = code.to_owned();
    copy.connect_clicked(move |button| button.clipboard().set_text(&raw));
    toolbar.append(&copy);
    root.append(&toolbar);
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .left_margin(6)
        .right_margin(6)
        .top_margin(4)
        .bottom_margin(4)
        .build();
    view.buffer().set_text(code);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&view)
        .propagate_natural_height(true)
        .max_content_height(340)
        .build();
    root.append(&scroller);
    root
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    fn streaming_keeps_unchanged_blocks_and_resolves_late_links() {
        let parent = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let mut blocks = Blocks::default();
        let start = "## Progress\n\nSee [the docs][help].\n\n```rust\nlet value = 1;\n```\n";
        blocks.render(&parent, start, true);
        let heading = parent.first_child().unwrap();
        let paragraph = heading.next_sibling().unwrap();
        let code = parent.last_child().unwrap();
        let complete = format!("{start}\n[help]: https://example.com\n\nDone.");
        blocks.render(&parent, &complete, false);

        assert_eq!(parent.first_child(), Some(heading.clone()));
        let linked = heading.next_sibling().unwrap();
        assert_ne!(linked, paragraph);
        let label = linked.first_child().and_downcast::<gtk::Label>().unwrap();
        assert!(label.label().contains("href=\"https://example.com\""));
        assert_eq!(linked.next_sibling(), Some(code));
        assert!(blocks.0.iter().all(|block| block.tree.is_empty()));
    }

    #[gtk::test]
    fn gfm_layout_keeps_table_cells_and_task_states() {
        let parent = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let markdown =
            "| Check | Count |\n| --- | ---: |\n| **Pass** | 3 |\n\n- [x] Verify\n- [ ] Ship";
        Blocks::default().render(&parent, markdown, false);
        let scroller = parent
            .first_child()
            .unwrap()
            .first_child()
            .and_downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let (_, height, ..) = scroller.measure(gtk::Orientation::Vertical, 500);
        assert!(height < 100, "two-row table measured {height}px");
        let grid = scroller
            .child()
            .and_downcast::<gtk::Viewport>()
            .unwrap()
            .child()
            .and_downcast::<gtk::Grid>()
            .unwrap();
        let pass = grid.child_at(0, 1).and_downcast::<gtk::Label>().unwrap();
        let count = grid.child_at(1, 1).and_downcast::<gtk::Label>().unwrap();
        assert_eq!(pass.label(), "<b>Pass</b>");
        assert_eq!(count.text(), "3");
        assert_eq!(count.xalign(), 1.0);
        let list = parent.last_child().unwrap();
        for (row, mark) in [(list.first_child(), "☑"), (list.last_child(), "☐")] {
            let label = row
                .unwrap()
                .first_child()
                .and_downcast::<gtk::Label>()
                .unwrap();
            assert_eq!(label.text(), mark);
        }
    }

    #[gtk::test]
    fn large_table_preserves_content_as_markdown() {
        let parent = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let markdown = format!(
            "| Key | Value |\n| --- | --- |\n{}| Last | End |",
            "| Row | OK |\n".repeat(256)
        );
        Blocks::default().render(&parent, &markdown, false);
        let code = parent
            .first_child()
            .unwrap()
            .first_child()
            .and_downcast::<gtk::Box>()
            .unwrap();
        let scroller = code
            .last_child()
            .and_downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let buffer = scroller
            .child()
            .and_downcast::<gtk::TextView>()
            .unwrap()
            .buffer();
        assert!(buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .contains("| Last | End |"));
    }
}
