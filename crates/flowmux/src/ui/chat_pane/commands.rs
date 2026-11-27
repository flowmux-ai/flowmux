// SPDX-License-Identifier: GPL-3.0-or-later

//! The `/` menu: the agent's own slash commands and skills.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::runtime::SlashCommand;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Owner {
    Agent,
    Skill,
}

#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub name: String,
    pub description: String,
    pub hint: String,
    pub owner: Owner,
}

impl Entry {
    /// Agent commands with arguments are completed so the arguments can follow.
    pub fn takes_input(&self) -> bool {
        !self.hint.is_empty()
    }
}

/// The commands and skills the agent reported, once per name.
pub(super) fn entries(agent: &[SlashCommand]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for command in agent {
        if valid_name(&command.name) && !entries.iter().any(|entry| entry.name == command.name) {
            entries.push(Entry {
                name: command.name.clone(),
                description: command.description.clone(),
                hint: command.hint.clone(),
                owner: if command.skill {
                    Owner::Skill
                } else {
                    Owner::Agent
                },
            });
        }
    }
    entries
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

/// The name being typed while the draft is still `/name` with no space.
pub(super) fn typed(draft: &str) -> Option<&str> {
    let name = draft.strip_prefix('/')?;
    (!name.contains(char::is_whitespace) && (name.is_empty() || valid_name(name))).then_some(name)
}

/// A draft's command name and arguments. Paths such as `/tmp/app.log` are not commands.
pub(super) fn parse(draft: &str) -> Option<(&str, &str)> {
    let rest = draft.strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    valid_name(name).then_some((name, args.trim()))
}

/// The exact name first, then names starting with the query, then names containing it.
pub(super) fn filter(entries: &[Entry], query: &str) -> Vec<Entry> {
    let query = query.to_lowercase();
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for entry in entries {
        let name = entry.name.to_lowercase();
        if name == query {
            starts.insert(0, entry.clone());
        } else if name.starts_with(&query) {
            starts.push(entry.clone());
        } else if name.contains(&query) {
            contains.push(entry.clone());
        }
    }
    starts.extend(contains);
    starts
}

/// The floating command list above the composer. Rows never take focus, so typing continues
/// in the composer while the menu is open.
pub(super) struct Menu {
    pub root: gtk::Revealer,
    list: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    shown: Rc<RefCell<Vec<Entry>>>,
    activate: Rc<RefCell<Option<Box<dyn Fn(Entry)>>>>,
    agent: String,
}

impl Menu {
    pub fn new(agent: &str) -> Self {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Browse);
        list.set_can_focus(false);
        list.set_activate_on_single_click(true);
        list.update_property(&[gtk::accessible::Property::Label("Slash commands")]);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(276)
            .build();
        let title = gtk::Label::new(Some(&format!("{agent} commands")));
        title.add_css_class("caption-heading");
        title.add_css_class("dim-label");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_width_chars(8);
        let keys = gtk::Label::new(Some("↑↓  Tab  Enter  Esc"));
        keys.set_ellipsize(gtk::pango::EllipsizeMode::End);
        keys.add_css_class("caption");
        keys.add_css_class("dim-label");
        keys.set_tooltip_text(Some(
            "↑↓ select · Tab complete · Enter run or complete · Esc close",
        ));
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("flowmux-chat-commands-header");
        header.append(&title);
        header.append(&keys);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
        card.add_css_class("flowmux-chat-commands");
        card.append(&header);
        card.append(&scroller);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(840);
        clamp.set_child(Some(&card));
        let root = gtk::Revealer::builder()
            .child(&clamp)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(120)
            .valign(gtk::Align::End)
            .build();
        let menu = Self {
            root,
            list,
            scroller,
            shown: Rc::new(RefCell::new(Vec::new())),
            activate: Rc::new(RefCell::new(None)),
            agent: agent.into(),
        };
        let activate = menu.activate.clone();
        let shown = menu.shown.clone();
        menu.list.connect_row_activated(move |_, row| {
            let entry = shown.borrow().get(row.index() as usize).cloned();
            if let (Some(entry), Some(activate)) = (entry, activate.borrow().as_ref()) {
                activate(entry);
            }
        });
        menu
    }

    pub fn connect_activate(&self, callback: impl Fn(Entry) + 'static) {
        *self.activate.borrow_mut() = Some(Box::new(callback));
    }

    pub fn is_open(&self) -> bool {
        self.root.reveals_child()
    }

    pub fn hide(&self) {
        self.root.set_reveal_child(false);
    }

    pub fn show(&self, entries: Vec<Entry>, query: &str) {
        self.list.remove_all();
        for entry in &entries {
            self.list.append(&row(entry, query, &self.agent));
        }
        let open = !entries.is_empty();
        *self.shown.borrow_mut() = entries;
        if open {
            self.list.select_row(self.list.row_at_index(0).as_ref());
            self.scroller.vadjustment().set_value(0.0);
        }
        self.root.set_reveal_child(open);
    }

    pub fn step(&self, delta: i32) {
        let count = self.shown.borrow().len() as i32;
        if count == 0 {
            return;
        }
        let current = self.list.selected_row().map_or(0, |row| row.index());
        let next = (current + delta).rem_euclid(count);
        let Some(row) = self.list.row_at_index(next) else {
            return;
        };
        self.list.select_row(Some(&row));
        let adjustment = self.scroller.vadjustment();
        if let Some(bounds) = row.compute_bounds(&self.list) {
            let top = f64::from(bounds.y());
            adjustment.clamp_page(top, top + f64::from(bounds.height()));
        }
    }

    pub fn selected(&self) -> Option<Entry> {
        let row = self.list.selected_row()?;
        self.shown.borrow().get(row.index() as usize).cloned()
    }
}

fn row(entry: &Entry, query: &str, agent: &str) -> gtk::ListBoxRow {
    let typed = entry.name.len().min(query.len());
    let name = gtk::Label::new(None);
    name.set_markup(&format!(
        "/<b>{}</b>{}",
        glib::markup_escape_text(&entry.name[..typed]),
        glib::markup_escape_text(&entry.name[typed..])
    ));
    name.add_css_class("flowmux-chat-command-name");
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    // A narrow menu keeps the names readable and shortens descriptions first.
    name.set_width_chars(entry.name.chars().count().min(9) as i32 + 1);
    name.set_max_width_chars(28);
    name.set_xalign(0.0);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.append(&name);
    if !entry.hint.is_empty() {
        let hint = gtk::Label::new(Some(&entry.hint));
        hint.add_css_class("flowmux-chat-command-hint");
        hint.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hint.set_max_width_chars(18);
        line.append(&hint);
    }
    let description = gtk::Label::new(Some(&entry.description));
    description.add_css_class("dim-label");
    description.set_ellipsize(gtk::pango::EllipsizeMode::End);
    description.set_hexpand(true);
    description.set_xalign(0.0);
    line.append(&description);
    let (badge, class) = match entry.owner {
        Owner::Agent => (agent, "agent"),
        Owner::Skill => ("Skill", "skill"),
    };
    let badge = gtk::Label::new(Some(badge));
    badge.add_css_class("flowmux-chat-command-badge");
    badge.add_css_class(class);
    badge.set_valign(gtk::Align::Center);
    line.append(&badge);
    let row = gtk::ListBoxRow::builder()
        .child(&line)
        .focusable(false)
        .tooltip_text(format!("/{} {}", entry.name, entry.description).trim_end())
        .build();
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "/{} · {} · {}",
        entry.name,
        entry.description,
        match entry.owner {
            Owner::Agent => agent,
            Owner::Skill => "Skill",
        }
    ))]);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str, skill: bool) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: format!("{name} from the agent"),
            hint: String::new(),
            skill,
            path: None,
        }
    }

    #[test]
    fn agent_commands_rank_exact_then_prefix_matches() {
        let all = entries(&[
            command("usage-credits", false),
            command("clear", false),
            command("usage", false),
            command("sclear", true),
            command("clear", true),
        ]);
        assert_eq!(all.iter().filter(|entry| entry.name == "clear").count(), 1);
        let names: Vec<_> = filter(&all, "cl")
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, ["clear", "sclear"]);
        // Enter on `/usage` must not pick a longer name listed earlier.
        assert_eq!(filter(&all, "usage")[0].name, "usage");
    }

    #[test]
    fn paths_and_sentences_are_not_commands() {
        assert_eq!(
            parse("/review focus on tests"),
            Some(("review", "focus on tests"))
        );
        assert_eq!(parse("/tmp/app.log 봐줘"), None);
        assert_eq!(typed("/rev"), Some("rev"));
        assert_eq!(typed("/"), Some(""));
        assert_eq!(typed("/review now"), None);
        assert_eq!(typed("plain"), None);
    }
}
