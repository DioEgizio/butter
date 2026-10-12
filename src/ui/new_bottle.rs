//! Creating a bottle: its directory, then its Rosemary prefix.

use adw::prelude::*;
use gtk::glib;

use crate::bottle::{self, Arch, Config, SystemFiles};
use crate::runner;
use crate::ui::{self, task, window::Window};

pub fn present(window: &Window) {
    let runners = runner::installed();
    if runners.is_empty() {
        let window = window.clone();
        glib::spawn_future_local(async move {
            if ui::confirm(
                &window.widget(),
                "No Runners Installed",
                "A bottle needs a runner, a build of Rosemary, to run anything. Install one first.",
                "_Show Runners",
                false,
            )
            .await
            {
                window.show_runners();
            }
        });
        return;
    }

    let name = adw::EntryRow::builder().title("Name").build();
    let labels: Vec<String> = runners.iter().map(|r| r.info.name.clone()).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let default = window.settings().default_runner;
    let selected = runners
        .iter()
        .position(|r| Some(&r.id) == default.as_ref())
        .unwrap_or(0) as u32;
    let runner_row = ui::combo_row("Runner", None, &labels, selected);

    let base = adw::SwitchRow::builder()
        .title("Install Darwin Base")
        .subtitle("Downloads the small (~100mb) MiniDarwin base, checked against hashes built into Rosemary. Needed for most apps to run.")
        .active(true)
        .build();
    let arch_labels: Vec<&str> = Arch::ALL.iter().map(|a| a.label()).collect();
    let arch = ui::combo_row(
        "Prefix Architecture",
        Some("Not usually necessary to override. Only for x86-64 applications that assume a fully x86-64 Darwin rootfs."),
        &arch_labels,
        0,
    );
    let policy_labels: Vec<&str> = SystemFiles::ALL.iter().map(|p| p.label()).collect();
    let policy = ui::combo_row(
        "File Integrity",
        Some("Determines if Rosemary will attempt to restore damaged system files on its own if an app modifies them on accident"),
        &policy_labels,
        0,
    );
    base.bind_property("active", &arch, "sensitive")
        .sync_create()
        .build();
    base.bind_property("active", &policy, "sensitive")
        .sync_create()
        .build();

    let general = adw::PreferencesGroup::new();
    general.add(&name);
    general.add(&runner_row);
    let system = adw::PreferencesGroup::builder()
        .title("System")
        .description("Rosemary gives every bottle a Darwin filesystem of its own")
        .build();
    system.add(&base);
    system.add(&arch);
    system.add(&policy);
    let page = adw::PreferencesPage::new();
    page.add(&general);
    page.add(&system);

    let cancel = gtk::Button::with_mnemonic("_Cancel");
    let create = gtk::Button::with_mnemonic("C_reate");
    create.add_css_class("suggested-action");
    create.set_sensitive(false);
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&create);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title("New Bottle")
        .content_width(480)
        .content_height(560)
        .child(&toolbar)
        .default_widget(&create)
        .focus_widget(&name)
        .build();

    name.connect_changed({
        let create = create.clone();
        move |entry| {
            let text = entry.text();
            let taken = bottle::list()
                .iter()
                .any(|b| b.config.name.eq_ignore_ascii_case(text.trim()));
            create.set_sensitive(!text.trim().is_empty() && !taken);
            if taken {
                entry.add_css_class("error");
            } else {
                entry.remove_css_class("error");
            }
        }
    });
    name.connect_entry_activated({
        let create = create.clone();
        move |_| {
            if create.is_sensitive() {
                create.emit_clicked();
            }
        }
    });
    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    create.connect_clicked({
        let dialog = dialog.clone();
        let window = window.clone();
        move |_| {
            let runner = runners
                .get(runner_row.selected() as usize)
                .map(|r| r.id.clone());
            let mut config = Config::new(name.text().trim(), runner);
            config.arch = Arch::from_index(arch.selected());
            config.system_files = SystemFiles::from_index(policy.selected());
            let with_base = base.is_active();
            dialog.close();
            let window = window.clone();
            glib::spawn_future_local(async move {
                match bottle::create(config) {
                    Ok(bottle) => {
                        window.refresh();
                        if create_prefix(&window, &bottle, with_base).await {
                            window.show_bottle(bottle);
                        }
                    }
                    Err(e) => window.error(format!("Could not create the bottle: {e}")),
                }
            });
        }
    });

    dialog.present(Some(&window.widget()));
}

/// Run `rosemary prefix create` for `bottle`; true when it worked.
pub async fn create_prefix(window: &Window, bottle: &bottle::Bottle, with_base: bool) -> bool {
    let key = format!("bottle:{}", bottle.id);
    if !window.begin(&key) {
        window.notify("This bottle is already busy");
        return false;
    }
    let command = bottle.prefix_command("create", &bottle.create_args(with_base));
    let result = match command {
        Ok(mut command) => {
            task::run(
                window,
                &format!("Creating {}", bottle.config.name),
                move |r| {
                    r.stage("Creating the prefix", None);
                    r.run_checked(&mut command)?;
                    Ok("The bottle is ready".into())
                },
            )
            .await
        }
        Err(e) => Err(e.to_string()),
    };
    window.end(&key);
    window.refresh();
    if let Err(e) = &result {
        window.error(format!("Could not create the prefix: {e}"));
    }
    result.is_ok()
}
