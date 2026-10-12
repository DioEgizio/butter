//! The list of bottles.

use adw::prelude::*;

use crate::ui::window::Window;
use crate::{bottle, runner, ui};

pub struct BottlesPage {
    root: gtk::Stack,
    list: gtk::ListBox,
    empty: adw::StatusPage,
    create: gtk::Button,
    install: gtk::Button,
    window: Window,
}

impl BottlesPage {
    pub fn new(window: &Window) -> BottlesPage {
        let list = ui::boxed_list();
        let group = adw::PreferencesGroup::builder().title("Bottles").build();
        group.add(&list);
        let page = adw::PreferencesPage::new();
        page.add(&group);

        let empty = adw::StatusPage::builder()
            .icon_name("package-x-generic-symbolic")
            .title("No Bottles")
            .vexpand(true)
            .build();
        let empty_buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        let create = gtk::Button::builder()
            .label("_Create Bottle")
            .use_underline(true)
            .action_name("win.new-bottle")
            .build();
        create.add_css_class("pill");
        create.add_css_class("suggested-action");
        let install = gtk::Button::builder()
            .label("_Install a Runner")
            .use_underline(true)
            .action_name("win.show-runners")
            .build();
        install.add_css_class("pill");
        empty_buttons.append(&create);
        empty_buttons.append(&install);
        empty.set_child(Some(&empty_buttons));

        let root = gtk::Stack::new();
        root.add_named(&empty, Some("empty"));
        root.add_named(&page, Some("list"));

        BottlesPage {
            root,
            list,
            empty,
            create,
            install,
            window: window.clone(),
        }
    }

    pub fn widget(&self) -> &gtk::Stack {
        &self.root
    }

    pub fn refresh(&self) {
        let bottles = bottle::list();
        let has_runner = !runner::installed().is_empty();
        self.empty.set_description(Some(if has_runner {
            "A bottle keeps a Mac program's files, settings and runner together"
        } else {
            "Bottles run Mac programs with Rosemary. Install a runner first, then create a bottle"
        }));
        self.create.set_sensitive(has_runner);
        self.install.set_visible(!has_runner);
        if has_runner {
            self.create.add_css_class("suggested-action");
            self.install.remove_css_class("suggested-action");
        } else {
            self.create.remove_css_class("suggested-action");
            self.install.add_css_class("suggested-action");
        }
        self.root
            .set_visible_child_name(if bottles.is_empty() { "empty" } else { "list" });

        self.list.remove_all();
        for bottle in bottles {
            let row = adw::ActionRow::builder()
                .title(glib_escape(&bottle.config.name))
                .subtitle(glib_escape(&subtitle(&bottle)))
                .activatable(true)
                .build();
            if !bottle.is_ready() || bottle.runner().is_none() {
                let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
                warning.set_tooltip_text(Some(if bottle.runner().is_none() {
                    "This bottle's runner is not installed"
                } else {
                    "This bottle's prefix has not been created"
                }));
                warning.add_css_class("warning");
                row.add_suffix(&warning);
            }
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let window = self.window.clone();
            row.connect_activated(move |_| {
                // Re-read, in case it changed since the list was drawn.
                match bottle::load(&bottle.id) {
                    Some(bottle) => window.show_bottle(bottle),
                    None => window.refresh(),
                }
            });
            self.list.append(&row);
        }
    }
}

fn subtitle(bottle: &bottle::Bottle) -> String {
    let runner = match bottle.runner() {
        Some(runner) => runner.info.name,
        None => match &bottle.config.runner {
            Some(id) => format!("{id} (missing)"),
            None => "No runner".into(),
        },
    };
    match bottle.config.programs.len() {
        0 => runner,
        1 => format!("{runner} · 1 program"),
        n => format!("{runner} · {n} programs"),
    }
}

/// Rows render titles as markup.
fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}
