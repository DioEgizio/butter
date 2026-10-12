//! One bottle: its programs, how Rosemary runs them, and its prefix.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::bottle::{
    self, Backend, Bottle, Decorations, Menu, PrefixHome, Program, SwapInterval, SystemFiles,
};
use crate::permissions::{self, Feature};
use crate::runner;
use crate::sessions::Status;
use crate::ui::{self, new_bottle, process_manager, program_dialog, task, window::Window};

#[derive(Clone)]
pub struct BottlePage(Rc<Inner>);

struct Inner {
    page: adw::NavigationPage,
    window: Window,
    bottle: RefCell<Bottle>,
    programs: gtk::ListBox,
    env: gtk::ListBox,
    banner: adw::Banner,
    /// A runner change asked for while programs were running, applied once
    /// they have all ended.
    pending_runner: RefCell<Option<Option<String>>>,
    /// Set while the Runner row is changed by Butter, not the user.
    syncing_runner: Cell<bool>,
}

impl BottlePage {
    pub fn new(window: &Window, bottle: Bottle) -> BottlePage {
        let name = bottle.config.name.clone();
        let programs = ui::boxed_list();
        let env = ui::boxed_list();
        let banner = adw::Banner::new("");

        let content = adw::PreferencesPage::new();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&content));
        let page = adw::NavigationPage::builder()
            .title(&name)
            .tag(format!("bottle:{}", bottle.id))
            .child(&toolbar)
            .build();

        let this = BottlePage(Rc::new(Inner {
            page,
            window: window.clone(),
            bottle: RefCell::new(bottle),
            programs,
            env,
            banner,
            pending_runner: RefCell::new(None),
            syncing_runner: Cell::new(false),
        }));

        let run = gtk::MenuButton::builder()
            .icon_name("media-playback-start-symbolic")
            .tooltip_text("Run…")
            .menu_model(&{
                let menu = gtk::gio::Menu::new();
                menu.append(Some("Run App _Bundle…"), Some("bottle.run-bundle"));
                menu.append(Some("Run _Executable…"), Some("bottle.run-executable"));
                menu
            })
            .build();
        header.pack_end(&run);
        let processes = gtk::Button::builder()
            .icon_name("utilities-terminal-symbolic")
            .tooltip_text("Process Manager")
            .build();
        let weak = this.weak();
        processes.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                this.window().show_processes(&this.bottle(), None);
            }
        });
        header.pack_end(&processes);
        header.pack_end(&process_manager::power_button(window, &this.bottle().id));
        this.setup_actions();

        // The banner offers a button only when the prefix is missing.
        let weak = this.weak();
        this.0.banner.connect_button_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            glib::spawn_future_local(async move {
                new_bottle::create_prefix(this.window(), &this.bottle(), true).await;
                this.refresh_banner();
            });
        });

        content.add(&this.programs_group());
        content.add(&this.runtime_group());
        content.add(&this.display_group());
        content.add(&this.advanced_group());
        content.add(&this.environment_group());
        content.add(&this.prefix_group());
        content.add(&this.danger_group());
        this.refresh_programs();
        this.refresh_environment();
        this.refresh_banner();
        this
    }

    pub fn widget(&self) -> &adw::NavigationPage {
        &self.0.page
    }

    fn bottle(&self) -> Bottle {
        self.0.bottle.borrow().clone()
    }

    fn window(&self) -> &Window {
        &self.0.window
    }

    /// Change the bottle's settings and save them.
    fn edit(&self, change: impl FnOnce(&mut bottle::Config)) {
        let mut bottle = self.0.bottle.borrow_mut();
        change(&mut bottle.config);
        if let Err(e) = bottle.save() {
            drop(bottle);
            self.window()
                .error(format!("Could not save the bottle: {e}"));
        }
    }

    fn weak(&self) -> std::rc::Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn setup_actions(&self) {
        let group = gtk::gio::SimpleActionGroup::new();
        for (name, bundle) in [("run-bundle", true), ("run-executable", false)] {
            let action = gtk::gio::SimpleAction::new(name, None);
            let weak = self.weak();
            action.connect_activate(move |_, _| {
                let Some(inner) = weak.upgrade() else { return };
                let this = BottlePage(inner);
                glib::spawn_future_local(async move {
                    if let Some(path) = program_dialog::choose_target(this.window(), bundle).await {
                        this.launch(&path, &[], Backend::Auto, &bottle::program_name(&path));
                    }
                });
            });
            group.add_action(&action);
        }
        self.0.page.insert_action_group("bottle", Some(&group));
    }

    fn refresh_banner(&self) {
        let bottle = self.bottle();
        let (text, button) = if bottle.runner().is_none() {
            ("This bottle's runner is not installed", None)
        } else if !bottle.is_ready() {
            ("This bottle's prefix has not been created", Some("Create"))
        } else {
            ("", None)
        };
        self.0.banner.set_title(text);
        self.0.banner.set_button_label(button);
        self.0.banner.set_revealed(!text.is_empty());
    }

    /// Start `target` in this bottle and report how it ends.
    fn launch(&self, target: &Path, args: &[String], backend: Backend, name: &str) {
        let bottle = self.bottle();
        let window = self.window().clone();
        let on_end = {
            let window = window.clone();
            let bottle = bottle.clone();
            move |session: &crate::sessions::Session| {
                let status = session.status();
                if matches!(&status, Status::Exited(s) if s.success()) {
                    return;
                }
                let toast = adw::Toast::builder()
                    .title(glib::markup_escape_text(&format!(
                        "{}: {}",
                        session.name,
                        status.describe()
                    )))
                    .button_label("Show Output")
                    .timeout(10)
                    .build();
                let (window2, id) = (window.clone(), session.id);
                toast.connect_button_clicked(move |_| {
                    window2.show_processes(&bottle, Some(id));
                });
                window.toast(toast);
            }
        };
        match window
            .sessions()
            .launch(&bottle, target, args, backend, name, on_end)
        {
            Ok(session) => {
                let toast = adw::Toast::builder()
                    .title(glib::markup_escape_text(&format!("Starting {name}…")))
                    .button_label("Show")
                    .build();
                let (window2, id) = (window.clone(), session.id);
                toast.connect_button_clicked(move |_| {
                    window2.show_processes(&bottle, Some(id));
                });
                window.toast(toast);
            }
            Err(e) => window.error(format!("Could not start {name}: {e:#}")),
        }
    }

    fn programs_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("Programs").build();
        let add = ui::icon_button("list-add-symbolic", "Add Program…");
        let weak = self.weak();
        add.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            let page = this.clone();
            program_dialog::present(this.window(), None, move |program| {
                page.edit(|c| c.programs.push(program));
                page.refresh_programs();
                page.window().refresh();
            });
        });
        group.set_header_suffix(Some(&add));
        group.add(&self.0.programs);
        group
    }

    fn refresh_programs(&self) {
        let list = &self.0.programs;
        list.remove_all();
        let programs = self.bottle().config.programs;
        if programs.is_empty() {
            list.append(&ui::placeholder_row(
                "Add a program to run it with one click",
            ));
        }
        for (index, program) in programs.into_iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&program.name))
                .subtitle(glib::markup_escape_text(
                    &program.path.display().to_string(),
                ))
                .subtitle_lines(1)
                .build();
            if !program.path.exists() {
                let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
                warning.set_tooltip_text(Some("Not found"));
                warning.add_css_class("warning");
                row.add_suffix(&warning);
            }
            let run = ui::icon_button("media-playback-start-symbolic", "Run");
            let edit = ui::icon_button("document-edit-symbolic", "Edit");
            let remove = ui::icon_button("user-trash-symbolic", "Remove");
            row.add_suffix(&run);
            row.add_suffix(&edit);
            row.add_suffix(&remove);
            row.set_activatable_widget(Some(&run));

            let weak = self.weak();
            let p = program.clone();
            run.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    BottlePage(inner).launch(&p.path, &p.args, p.backend, &p.name);
                }
            });
            let weak = self.weak();
            edit.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                let this = BottlePage(inner);
                let page = this.clone();
                program_dialog::present(
                    this.window(),
                    Some(program.clone()),
                    move |changed: Program| {
                        page.edit(|c| {
                            if let Some(slot) = c.programs.get_mut(index) {
                                *slot = changed;
                            }
                        });
                        page.refresh_programs();
                    },
                );
            });
            let weak = self.weak();
            remove.connect_clicked(move |_| {
                let Some(inner) = weak.upgrade() else { return };
                let this = BottlePage(inner);
                this.edit(|c| {
                    if index < c.programs.len() {
                        c.programs.remove(index);
                    }
                });
                this.refresh_programs();
                this.window().refresh();
            });
            list.append(&row);
        }
    }

    /// A combo row bound to one of the bottle's choices.
    fn choice<T: Copy + 'static>(
        &self,
        title: &str,
        subtitle: Option<&str>,
        all: &'static [T],
        label: fn(T) -> &'static str,
        get: fn(&bottle::Config) -> T,
        set: fn(&mut bottle::Config, T),
    ) -> adw::ComboRow {
        let labels: Vec<&str> = all.iter().map(|v| label(*v)).collect();
        let current = get(&self.bottle().config);
        let selected = all
            .iter()
            .position(|v| label(*v) == label(current))
            .unwrap_or(0) as u32;
        let row = ui::combo_row(title, subtitle, &labels, selected);
        let weak = self.weak();
        row.connect_selected_notify(move |row| {
            if let (Some(inner), Some(value)) = (weak.upgrade(), all.get(row.selected() as usize)) {
                BottlePage(inner).edit(|c| set(c, *value));
            }
        });
        row
    }

    /// A switch row bound to one of the bottle's flags.
    fn switch(
        &self,
        title: &str,
        subtitle: &str,
        get: fn(&bottle::Config) -> bool,
        set: fn(&mut bottle::Config, bool),
    ) -> adw::SwitchRow {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .active(get(&self.bottle().config))
            .build();
        let weak = self.weak();
        row.connect_active_notify(move |row| {
            if let Some(inner) = weak.upgrade() {
                let active = row.is_active();
                BottlePage(inner).edit(|c| set(c, active));
            }
        });
        row
    }

    /// Show the bottle's current runner in `row` without treating it as a
    /// choice by the user.
    fn select_runner(&self, row: &adw::ComboRow, ids: &[Option<String>]) {
        let current = self.bottle().config.runner;
        if let Some(index) = ids.iter().position(|id| *id == current) {
            self.0.syncing_runner.set(true);
            row.set_selected(index as u32);
            self.0.syncing_runner.set(false);
        }
    }

    fn runtime_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("Runtime").build();

        let runners = runner::installed();
        let current = self.bottle().config.runner;
        let mut labels: Vec<String> = runners.iter().map(|r| r.info.name.clone()).collect();
        let mut ids: Vec<Option<String>> = runners.iter().map(|r| Some(r.id.clone())).collect();
        if let Some(id) = &current
            && !ids.contains(&Some(id.clone()))
        {
            labels.push(format!("{id} (not installed)"));
            ids.push(Some(id.clone()));
        }
        if current.is_none() {
            labels.push("None".into());
            ids.push(None);
        }
        let selected = ids.iter().position(|id| *id == current).unwrap_or(0) as u32;
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let runner_row = ui::combo_row(
            "Runner",
            Some("The version of Rosemary to run this bottle with"),
            &label_refs,
            selected,
        );
        let ids = Rc::new(ids);

        // Applies the pending change once nothing runs in the bottle.
        let weak = self.weak();
        let (row, all) = (runner_row.clone(), ids.clone());
        self.window().sessions().connect_changed(move || {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            let this = BottlePage(inner);
            let pending = this.0.pending_runner.borrow().clone();
            let Some(id) = pending else { return true };
            if this.window().sessions().is_running(&this.bottle().id) {
                return true;
            }
            this.0.pending_runner.borrow_mut().take();
            this.edit(|c| c.runner = id.clone());
            this.select_runner(&row, &all);
            this.refresh_banner();
            this.window().refresh();
            this.window().notify("Applied the new runner");
            true
        });

        let weak = self.weak();
        let all = ids.clone();
        runner_row.connect_selected_notify(move |row| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            if this.0.syncing_runner.get() {
                return;
            }
            let Some(id) = all.get(row.selected() as usize).cloned() else {
                return;
            };
            if id == this.bottle().config.runner {
                this.0.pending_runner.borrow_mut().take();
                return;
            }
            let sessions = this.window().sessions();
            if !sessions.is_running(&this.bottle().id) {
                this.0.pending_runner.borrow_mut().take();
                this.edit(|c| c.runner = id);
                this.refresh_banner();
                this.window().refresh();
                return;
            }
            // Programs still run with the current runner: keep it, and
            // switch once they are gone.
            *this.0.pending_runner.borrow_mut() = Some(id);
            this.select_runner(row, &all);
            this.window()
                .error("Still running: hit power, then Stop. Runner changes after.");
            let weak = this.weak();
            glib::timeout_add_seconds_local(2, move || {
                let Some(inner) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let this = BottlePage(inner);
                if this.0.pending_runner.borrow().is_none() {
                    return glib::ControlFlow::Break;
                }
                this.window().sessions().poll();
                glib::ControlFlow::Continue
            });
        });
        group.add(&runner_row);

        group.add(&self.choice(
            "Backend",
            Some("Rosemary backend to run programs with"),
            Backend::ALL,
            Backend::label,
            |c| c.backend,
            |c, v| c.backend = v,
        ));
        group.add(&self.choice(
            "Home Folder",
            Some("Where programs keep documents and settings"),
            PrefixHome::ALL,
            PrefixHome::label,
            |c| c.prefix_home,
            |c, v| c.prefix_home = v,
        ));
        group.add(&self.switch(
            "Disable Metal Support",
            "Hides the Vulkan driver from Rosemary. This may help with older hardware and applications where Metal translation doesn't work properly yet",
            |c| c.disable_metal,
            |c, v| c.disable_metal = v,
        ));
        group.add(&self.steam_switch());
        group
    }

    /// The Steam bridge, which only works when the sandbox can reach Steam's
    /// folders. Switching it on without them shows how to allow them.
    fn steam_switch(&self) -> adw::SwitchRow {
        let row = self.switch(
            "Steam Integration",
            "Bridge Steamworks calls to the Steam client on this computer",
            |c| c.steam,
            |c, v| c.steam = v,
        );
        if permissions::available(Feature::Steam) {
            return row;
        }
        row.set_subtitle(
            "Bridge Steamworks calls to the Steam client on this computer. \
             Butter needs permission to reach Steam's folders first",
        );
        let warning = ui::icon_button("dialog-warning-symbolic", "Allow Access to Steam");
        warning.set_visible(row.is_active());
        let weak = self.weak();
        warning.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                ui::permissions::request(BottlePage(inner).window(), Feature::Steam);
            }
        });
        row.add_suffix(&warning);
        let weak = self.weak();
        row.connect_active_notify(move |row| {
            warning.set_visible(row.is_active());
            if row.is_active()
                && let Some(inner) = weak.upgrade()
            {
                ui::permissions::request(BottlePage(inner).window(), Feature::Steam);
            }
        });
        row
    }

    fn display_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Display and Input")
            .build();
        group.add(&self.choice(
            "Window Titlebars",
            None,
            Decorations::ALL,
            Decorations::label,
            |c| c.decorations,
            |c, v| c.decorations = v,
        ));
        group.add(&self.choice(
            "Menu Bar",
            Some("Where an application's main menu is shown"),
            Menu::ALL,
            Menu::label,
            |c| c.menu,
            |c, v| c.menu = v,
        ));
        group.add(&self.choice(
            "OpenGL Sync",
            Some("Override the swap interval of OpenGL programs"),
            SwapInterval::ALL,
            SwapInterval::label,
            |c| c.swap_interval,
            |c, v| c.swap_interval = v,
        ));
        group.add(&self.switch(
            "Control as Command",
            "Let Ctrl stand in for ⌘ in menu shortcuts",
            |c| c.menu_ctrl,
            |c, v| c.menu_ctrl = v,
        ));
        group.add(&self.switch(
            "Confine Pointer",
            "Keep the pointer inside the focused window; some ports need it for dragging",
            |c| c.confine_pointer,
            |c, v| c.confine_pointer = v,
        ));
        group
    }

    fn advanced_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Advanced")
            .description("Experimental options. Change these only to work around a problem")
            .build();
        let expander = adw::ExpanderRow::builder()
            .title("Emulator and Runtime")
            .build();
        expander.add_row(&self.switch(
            "Process Server",
            "Let programs in this bottle see each other, for ps, top and task_for_pid",
            |c| c.process_server,
            |c, v| c.process_server = v,
        ));
        expander.add_row(&self.switch(
            "Dead Flag Elimination",
            "Faster in some programs, but may be inaccurate",
            |c| c.dead_flags,
            |c, v| c.dead_flags = v,
        ));
        expander.add_row(&self.switch(
            "Native Call Return",
            "May speed up some emulated programs",
            |c| c.call_return,
            |c, v| c.call_return = v,
        ));
        expander.add_row(&self.switch(
            "Strict Imports",
            "Refuse to run programs with unresolved imports",
            |c| c.strict_imports,
            |c, v| c.strict_imports = v,
        ));
        group.add(&expander);
        group
    }

    fn environment_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Environment Variables")
            .description(
                "Passed to every program in this bottle. These override the settings above",
            )
            .build();
        let entry = adw::EntryRow::builder()
            .title("NAME=value")
            .show_apply_button(true)
            .build();
        let weak = self.weak();
        entry.connect_apply(move |entry| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            let text = entry.text();
            let Some((key, value)) = text.split_once('=') else {
                this.window().notify("Write variables as NAME=value");
                return;
            };
            let key = key.trim().to_string();
            if !bottle::valid_env_name(&key) {
                this.window()
                    .notify("Names use letters, digits and underscores");
                return;
            }
            let value = value.to_string();
            this.edit(|c| {
                c.env.insert(key, value);
            });
            entry.set_text("");
            this.refresh_environment();
        });
        group.add(&self.0.env);
        let add = ui::boxed_list();
        add.append(&entry);
        add.set_margin_top(12);
        group.add(&add);
        group
    }

    fn refresh_environment(&self) {
        let list = &self.0.env;
        list.remove_all();
        let env = self.bottle().config.env;
        list.set_visible(!env.is_empty());
        for (key, value) in env {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&key))
                .subtitle(glib::markup_escape_text(&value))
                .subtitle_selectable(true)
                .build();
            row.add_css_class("property");
            let remove = ui::icon_button("user-trash-symbolic", "Remove");
            let weak = self.weak();
            remove.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    let this = BottlePage(inner);
                    this.edit(|c| {
                        c.env.remove(&key);
                    });
                    this.refresh_environment();
                }
            });
            row.add_suffix(&remove);
            list.append(&row);
        }
    }

    /// Run `rosemary prefix ACTION` for this bottle in a task dialog.
    fn prefix_task(&self, title: &str, action: &'static str, args: Vec<&'static str>) {
        let this = self.clone();
        let title = title.to_string();
        glib::spawn_future_local(async move {
            let bottle = this.bottle();
            let key = format!("bottle:{}", bottle.id);
            if !this.window().begin(&key) {
                this.window().notify("This bottle is already busy");
                return;
            }
            match bottle.prefix_command(action, &args) {
                Ok(mut command) => {
                    let _ = task::run(this.window(), &title, move |r| {
                        r.stage(format!("rosemary prefix {action}"), None);
                        r.run_checked(&mut command)?;
                        Ok("Done".into())
                    })
                    .await;
                }
                Err(e) => this.window().error(e),
            }
            this.window().end(&key);
            this.refresh_banner();
        });
    }

    fn prefix_group(&self) -> adw::PreferencesGroup {
        let bottle = self.bottle();
        let group = adw::PreferencesGroup::builder()
            .title("Prefix")
            .description("The Darwin filesystem programs in this bottle see as /")
            .build();

        let location = adw::ActionRow::builder()
            .title("Location")
            .subtitle(glib::markup_escape_text(
                &bottle.prefix().display().to_string(),
            ))
            .subtitle_selectable(true)
            .build();
        let open = ui::icon_button("folder-open-symbolic", "Open in Files");
        let weak = self.weak();
        open.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                let bottle = this.bottle();
                let target = if bottle.drive().is_dir() {
                    bottle.drive()
                } else {
                    bottle.dir.clone()
                };
                ui::open_folder(&this.window().widget(), &target);
            }
        });
        location.add_suffix(&open);
        group.add(&location);

        let size = adw::ActionRow::builder()
            .title("Disk Usage")
            .subtitle("Calculating…")
            .build();
        let dir = bottle.dir.clone();
        let size_row = size.clone();
        glib::spawn_future_local(async move {
            let bytes = crate::process::background(move || crate::util::dir_size(&dir)).await;
            size_row.set_subtitle(&crate::util::format_size(bytes));
        });
        group.add(&size);

        let policy = self.choice(
            "System Files",
            Some("When a program changes a file the base owns"),
            SystemFiles::ALL,
            SystemFiles::label,
            |c| c.system_files,
            |c, v| c.system_files = v,
        );
        let weak = self.weak();
        // Rosemary records the policy in the prefix; re-provision to change it.
        policy.connect_selected_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                if this.bottle().is_ready() {
                    let args = this.bottle().create_args(true);
                    this.prefix_task("Changing System File Policy", "create", args);
                }
            }
        });
        group.add(&policy);

        let maintenance = adw::ExpanderRow::builder()
            .title("Maintenance")
            .subtitle("Check and repair the Darwin base")
            .build();
        let actions: [(&str, &str, &str, &'static str, Vec<&'static str>); 5] = [
            (
                "Details",
                "",
                "Prefix Details",
                "info",
                vec![],
            ),
            (
                "Verify",
                "Hash every system file against the manifest",
                "Verifying System Files",
                "verify",
                vec![],
            ),
            (
                "Repair",
                "Put back system files that differ without touching user-made system files",
                "Repairing System Files",
                "repair",
                vec![],
            ),
            (
                "Reset System Files",
                "Completely wipe anything unexpected in system files. Possibly destructive, use as a last resort.",
                "Resetting System Files",
                "reset-system",
                vec![],
            ),
            (
                "Update Base",
                "Move to the base release the runner pins",
                "Updating the Darwin Base",
                "update",
                vec![],
            ),
        ];
        for (title, subtitle, task_title, action, args) in actions {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(subtitle)
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = self.weak();
            row.connect_activated(move |_| {
                if let Some(inner) = weak.upgrade() {
                    BottlePage(inner).prefix_task(task_title, action, args.clone());
                }
            });
            maintenance.add_row(&row);
        }
        group.add(&maintenance);

        let log = adw::ActionRow::builder()
            .title("Last Run Log")
            .subtitle("Output of the most recently started program")
            .activatable(true)
            .build();
        log.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = self.weak();
        log.connect_activated(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = BottlePage(inner);
                let text = std::fs::read_to_string(this.bottle().last_log())
                    .unwrap_or_else(|_| "Nothing has run in this bottle yet.".into());
                ui::show_text(&this.window().widget(), "Last Run", &text);
            }
        });
        group.add(&log);
        group
    }

    fn danger_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let delete = gtk::Button::builder()
            .label("_Delete Bottle")
            .use_underline(true)
            .halign(gtk::Align::Center)
            .build();
        delete.add_css_class("pill");
        delete.add_css_class("destructive-action");
        let weak = self.weak();
        delete.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let this = BottlePage(inner);
            glib::spawn_future_local(async move {
                let bottle = this.bottle();
                let window = this.window().clone();
                if window.sessions().is_running(&bottle.id) {
                    window.error(format!(
                        "Programs are still running in {}; stop the bottle first",
                        bottle.config.name
                    ));
                    return;
                }
                if !ui::confirm(
                    &window.widget(),
                    &format!("Delete {}?", bottle.config.name),
                    "Its prefix, and every file programs saved inside it, will be permanently deleted.",
                    "_Delete",
                    true,
                )
                .await
                {
                    return;
                }
                let result = crate::process::background(move || bottle::delete(&bottle)).await;
                match result {
                    Ok(()) => {
                        window.pop_to_main();
                        window.refresh();
                        window.notify("Bottle deleted");
                    }
                    Err(e) => window.error(format!("Could not delete the bottle: {e}")),
                }
            });
        });
        group.add(&delete);
        group
    }
}
