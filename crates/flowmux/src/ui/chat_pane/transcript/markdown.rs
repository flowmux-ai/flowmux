// SPDX-License-Identifier: GPL-3.0-or-later

use comrak::nodes::{AstNode, ListType, NodeValue};
use comrak::{parse_document, Arena, Options};
use gtk::prelude::*;

use super::{label, text_label};

pub(super) fn render(box_: &gtk::Box, markdown: &str) {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    let document = parse_document(&arena, markdown, &options);
    for node in document.children() {
        block(box_, node, 0);
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
                let mark = if list.list_type == ListType::Ordered {
                    format!("{}.", list.start + index)
                } else {
                    "•".into()
                };
                let marker = label(&mark);
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
    let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
    copy.set_tooltip_text(Some("Copy code"));
    copy.update_property(&[gtk::accessible::Property::Label("Copy code")]);
    copy.add_css_class("flat");
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
