//! A bottle's process manager: its programs and every process they started,
//! each program's terminal, and controls to signal and stop them.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use vte4::prelude::*;

use crate::bottle::{self, Bottle};
use crate::procs::{self, Id};
use crate::sessions::{Entry, Session};
use crate::ui::{self, window::Window};

/// What a row in the sidebar stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    /// A program Butter started, whether or not it still runs.
    Program(u64),
    /// Another process a program started.
    Process(u64, Id),
}

struct Row {
    key: Key,
    /// Whether its program's keeper still ran when the row was made.
    alive: bool,
    row: gtk::ListBoxRow,
    title: gtk::Label,
    subtitle: gtk::Label,
    search: String,
}

/// What the focused row stands for now, from the latest snapshot.
struct Focus {
    session: Rc<Session>,
    name: String,
    /// The process controls act on, while it runs.
    target: Option<Id>,
    state: Option<char>,
    on_terminal: bool,
    is_program: bool,
}

#[derive(Clone)]
pub struct ProcessManager(Rc<Inner>);

struct Inner {
    page: adw::NavigationPage,
    window: Window,
    bottle: String,
    split: adw::NavigationSplitView,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
    rows: RefCell<Vec<Row>>,
    entries: RefCell<Vec<Entry>>,
    selected: Cell<Option<Key>>,
    /// Set while rows are rebuilt, when selection changes are not the user's.
    syncing: Cell<bool>,
    title: adw::WindowTitle,
    banner: adw::Banner,
    stack: gtk::Stack,
    terminals: gtk::Stack,
    terminal_widgets: RefCell<HashMap<u64, vte4::Terminal>>,
    signals: gtk::MenuButton,
    stop: gtk::Button,
    stop_badge: gtk::Image,
    /// Processes already asked to quit with the stop button: pressing it
    /// again kills them.
    asked: RefCell<HashSet<Id>>,
}

impl ProcessManager {
    pub fn new(window: &Window, bottle: &Bottle) -> ProcessManager {
        // The sidebar: every process, with a search entry.
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search processes")
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(6)
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        list.add_css_class("navigation-sidebar");
        let empty = gtk::Label::builder()
            .label("Nothing is running")
            .margin_top(24)
            .margin_bottom(24)
            .wrap(true)
            .build();
        empty.add_css_class("dim-label");
        list.set_placeholder(Some(&empty));
        let sidebar_header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new("Processes", &bottle.config.name))
            .build();
        let clear = ui::icon_button("edit-clear-all-symbolic", "Clear Exited Programs");
        sidebar_header.pack_end(&power_button(window, &bottle.id));
        sidebar_header.pack_end(&clear);
        let sidebar_toolbar = adw::ToolbarView::new();
        sidebar_toolbar.add_top_bar(&sidebar_header);
        sidebar_toolbar.add_top_bar(&search);
        sidebar_toolbar.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .build(),
        ));
        let sidebar = adw::NavigationPage::new(&sidebar_toolbar, "Processes");

        // The focused process: its controls and its program's terminal.
        let title = adw::WindowTitle::new("", "");
        let stop_badge = gtk::Image::builder()
            .icon_name("dialog-warning-symbolic")
            .pixel_size(10)
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .visible(false)
            .build();
        stop_badge.add_css_class("warning");
        let stop_icon = gtk::Overlay::builder()
            .child(&gtk::Image::from_icon_name("media-playback-stop-symbolic"))
            .build();
        stop_icon.add_overlay(&stop_badge);
        let stop = gtk::Button::builder()
            .child(&stop_icon)
            .tooltip_text("Stop")
            .build();
        let signals = gtk::MenuButton::builder()
            .icon_name("mail-send-symbolic")
            .always_show_arrow(true)
            .tooltip_text("Send Signal")
            .menu_model(&signal_menu())
            .build();
        // Its back button leads to the list, so only when they are not side by side.
        let header = adw::HeaderBar::builder()
            .title_widget(&title)
            .show_back_button(false)
            .build();
        header.pack_end(&stop);
        header.pack_end(&signals);
        let banner = adw::Banner::new("");

        let terminals = gtk::Stack::new();
        let stack = gtk::Stack::new();
        stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("utilities-terminal-symbolic")
                .title("No Process Selected")
                .description("Choose a process to see its output and send it input")
                .build(),
            Some("empty"),
        );
        stack.add_named(&terminals, Some("terminal"));
        stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("utilities-terminal-symbolic")
                .title("Not Connected to a Terminal")
                .description(
                    "This process does not write to its program's terminal, so there is no output \
                     to show. It can still be sent signals.",
                )
                .build(),
            Some("detached"),
        );
        stack.set_visible_child_name("empty");
        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&header);
        content_toolbar.add_top_bar(&banner);
        content_toolbar.set_content(Some(&stack));
        let content = adw::NavigationPage::builder()
            .title("Process")
            .tag("process")
            .child(&content_toolbar)
            .build();

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar)
            .content(&content)
            .min_sidebar_width(240.0)
            .max_sidebar_width(320.0)
            .build();
        let bin = adw::BreakpointBin::builder()
            .child(&split)
            .width_request(360)
            .height_request(300)
            .build();
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        breakpoint.add_setter(&header, "show-back-button", Some(&true.to_value()));
        bin.add_breakpoint(breakpoint);
        let page = adw::NavigationPage::builder()
            .title(format!("{} Processes", bottle.config.name))
            .tag(tag(&bottle.id))
            .child(&bin)
            .build();

        let this = ProcessManager(Rc::new(Inner {
            page,
            window: window.clone(),
            bottle: bottle.id.clone(),
            split,
            list,
            search,
            rows: RefCell::default(),
            entries: RefCell::default(),
            selected: Cell::new(None),
            syncing: Cell::new(false),
            title,
            banner,
            stack,
            terminals,
            terminal_widgets: RefCell::default(),
            signals,
            stop,
            stop_badge,
            asked: RefCell::default(),
        }));
        this.connect_signals(&clear);
        this.refresh();
        this
    }

    pub fn widget(&self) -> &adw::NavigationPage {
        &self.0.page
    }

    fn window(&self) -> &Window {
        &self.0.window
    }

    fn weak(&self) -> std::rc::Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    /// Show a program's terminal.
    pub fn focus(&self, session: u64) {
        self.0.selected.set(Some(Key::Program(session)));
        self.refresh();
        self.0.split.set_show_content(true);
    }

    fn connect_signals(&self, clear: &gtk::Button) {
        let weak = self.weak();
        self.0.list.set_filter_func(move |row| {
            let Some(inner) = weak.upgrade() else {
                return true;
            };
            let query = inner.search.text().to_lowercase();
            let rows = inner.rows.borrow();
            query.is_empty()
                || usize::try_from(row.index())
                    .ok()
                    .and_then(|i| rows.get(i))
                    .is_none_or(|r| r.search.contains(query.trim()))
        });
        let weak = self.weak();
        self.0.search.connect_search_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.list.invalidate_filter();
            }
        });

        let weak = self.weak();
        self.0.list.connect_row_selected(move |_, row| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.syncing.get() {
                return;
            }
            let key = row.and_then(|row| {
                let index = usize::try_from(row.index()).ok()?;
                inner.rows.borrow().get(index).map(|r| r.key)
            });
            let this = ProcessManager(inner);
            this.0.selected.set(key);
            this.show();
            if key.is_some() {
                this.0.split.set_show_content(true);
            }
        });
        // Choosing the selected row again, in the narrow layout.
        let weak = self.weak();
        self.0.list.connect_row_activated(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                inner.split.set_show_content(true);
            }
        });

        let weak = self.weak();
        clear.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.window.sessions().dismiss_ended(&inner.bottle);
            }
        });

        let weak = self.weak();
        self.0.stop.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                let this = ProcessManager(inner);
                glib::spawn_future_local(async move { this.stop().await });
            }
        });

        let group = gio::SimpleActionGroup::new();
        let action = gio::SimpleAction::new("signal", Some(glib::VariantTy::INT32));
        let weak = self.weak();
        action.connect_activate(move |_, parameter| {
            let (Some(inner), Some(signal)) = (weak.upgrade(), parameter.and_then(|p| p.get()))
            else {
                return;
            };
            ProcessManager(inner).send(signal);
        });
        group.add_action(&action);
        self.0.page.insert_action_group("process", Some(&group));

        // Follow the bottle: when programs start or end, and every second
        // for the processes they start.
        let weak = self.weak();
        self.window()
            .sessions()
            .connect_changed(move || match weak.upgrade() {
                Some(inner) => {
                    ProcessManager(inner).refresh();
                    true
                }
                None => false,
            });
        let weak = self.weak();
        glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(inner) => {
                if inner.page.is_mapped() {
                    ProcessManager(inner).refresh();
                }
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });

        // Terminals follow the light or dark style.
        let weak = self.weak();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                for terminal in inner.terminal_widgets.borrow().values() {
                    style_terminal(terminal);
                }
            }
        });
    }

    /// Read the processes again and update the list and the focused one.
    fn refresh(&self) {
        let sessions = self.window().sessions();
        sessions.poll();
        let entries = sessions.snapshot(&self.0.bottle);
        let keys: Vec<Key> = entries.iter().map(key).collect();
        // Rows of programs that end gain a button to clear them.
        let same = {
            let rows = self.0.rows.borrow();
            rows.len() == keys.len()
                && rows
                    .iter()
                    .zip(&entries)
                    .all(|(r, e)| r.key == key(e) && r.alive == e.session.is_alive())
        };
        if same {
            for (row, entry) in self.0.rows.borrow_mut().iter_mut().zip(&entries) {
                let (title, subtitle, search) = describe(entry);
                row.title.set_label(&title);
                row.subtitle.set_label(&subtitle);
                row.search = search;
            }
        } else {
            self.rebuild(&entries);
        }

        // A process that ended leaves its program focused.
        if let Some(Key::Process(session, _)) = self.0.selected.get()
            && !keys.contains(&self.0.selected.get().unwrap())
        {
            self.0.selected.set(Some(Key::Program(session)));
        }
        if self.0.selected.get().is_some_and(|k| !keys.contains(&k)) {
            self.0.selected.set(None);
        }
        *self.0.entries.borrow_mut() = entries;
        self.select_row();
        self.0.list.invalidate_filter();
        self.show();
    }

    fn rebuild(&self, entries: &[Entry]) {
        self.0.syncing.set(true);
        self.0.list.remove_all();
        let mut rows = Vec::new();
        for entry in entries {
            let (title_text, subtitle_text, search) = describe(entry);
            let icon = gtk::Image::from_icon_name(if entry.is_program {
                "application-x-executable-symbolic"
            } else {
                "system-run-symbolic"
            });
            let title = gtk::Label::builder()
                .label(&title_text)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let subtitle = gtk::Label::builder()
                .label(&subtitle_text)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            subtitle.add_css_class("dim-label");
            subtitle.add_css_class("caption");
            let labels = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .valign(gtk::Align::Center)
                .hexpand(true)
                .build();
            labels.append(&title);
            labels.append(&subtitle);
            let content = gtk::Box::builder()
                .spacing(10)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(4 + 18 * entry.level.min(6) as i32)
                .build();
            content.append(&icon);
            content.append(&labels);
            if entry.is_program && !entry.session.is_alive() {
                labels.add_css_class("dim-label");
                let dismiss = ui::icon_button("window-close-symbolic", "Clear");
                dismiss.add_css_class("circular");
                let weak = self.weak();
                let id = entry.session.id;
                dismiss.connect_clicked(move |_| {
                    if let Some(inner) = weak.upgrade() {
                        inner.window.sessions().dismiss(id);
                    }
                });
                content.append(&dismiss);
            }
            let row = gtk::ListBoxRow::builder().child(&content).build();
            if let Some(process) = &entry.process {
                row.set_tooltip_text(Some(&process.command));
            }
            self.0.list.append(&row);
            rows.push(Row {
                key: key(entry),
                alive: entry.session.is_alive(),
                row,
                title,
                subtitle,
                search,
            });
        }
        *self.0.rows.borrow_mut() = rows;
        self.0.syncing.set(false);
    }

    fn select_row(&self) {
        self.0.syncing.set(true);
        let selected = self.0.selected.get();
        let row = self
            .0
            .rows
            .borrow()
            .iter()
            .find(|r| Some(r.key) == selected)
            .map(|r| r.row.clone());
        match row {
            Some(row) => self.0.list.select_row(Some(&row)),
            None => self.0.list.unselect_all(),
        }
        self.0.syncing.set(false);
    }

    fn focused(&self) -> Option<Focus> {
        let selected = self.0.selected.get()?;
        let entries = self.0.entries.borrow();
        let entry = entries.iter().find(|e| key(e) == selected)?;
        let process = entry.process.as_ref();
        Some(Focus {
            session: entry.session.clone(),
            name: describe(entry).0,
            target: process.map(|p| p.id),
            state: process.map(|p| p.state),
            on_terminal: process.is_some_and(|p| p.on_terminal),
            is_program: entry.is_program,
        })
    }

    /// Show the focused process.
    fn show(&self) {
        let inner = &self.0;
        let Some(focus) = self.focused() else {
            inner.stack.set_visible_child_name("empty");
            inner.title.set_title("");
            inner.title.set_subtitle("");
            inner.banner.set_revealed(false);
            inner.signals.set_sensitive(false);
            inner.stop.set_sensitive(false);
            inner.stop_badge.set_visible(false);
            return;
        };
        let session = &focus.session;
        let status = session.status();
        inner.title.set_title(&focus.name);
        inner
            .title
            .set_subtitle(&match (focus.target, focus.state) {
                (Some(id), Some(state)) => {
                    format!("PID {} · {}", id.pid, procs::state_label(state))
                }
                _ => status.describe(),
            });

        // Programs show their terminal, even after they end; other
        // processes only if they use it.
        let terminal = focus.is_program || focus.on_terminal;
        if terminal {
            let widget = self.terminal(session);
            widget.set_input_enabled(session.takes_input());
            inner
                .terminals
                .set_visible_child_name(&session.id.to_string());
            inner.stack.set_visible_child_name("terminal");
        } else {
            inner.stack.set_visible_child_name("detached");
        }

        let banner = if !status.is_running() && focus.is_program {
            Some(status.describe())
        } else if !session.is_linked() && terminal {
            Some(
                "Started before Butter was last opened: output is read from its log, and it \
                 takes no input"
                    .to_string(),
            )
        } else {
            None
        };
        inner
            .banner
            .set_title(banner.as_deref().unwrap_or_default());
        inner.banner.set_revealed(banner.is_some());

        inner.signals.set_sensitive(focus.target.is_some());
        inner.stop.set_sensitive(focus.target.is_some());
        let asked = focus
            .target
            .is_some_and(|id| inner.asked.borrow().contains(&id));
        inner.stop_badge.set_visible(asked);
        inner
            .stop
            .set_tooltip_text(Some(if asked { "Force Stop" } else { "Stop" }));
    }

    /// The terminal showing `session`'s output, made the first time.
    fn terminal(&self, session: &Rc<Session>) -> vte4::Terminal {
        if let Some(terminal) = self.0.terminal_widgets.borrow().get(&session.id) {
            return terminal.clone();
        }
        let terminal = vte4::Terminal::builder()
            .hexpand(true)
            .vexpand(true)
            .scrollback_lines(10_000)
            .scroll_on_output(false)
            .scroll_on_keystroke(true)
            .build();
        style_terminal(&terminal);
        terminal.feed(&session.output());

        let weak = terminal.downgrade();
        session.watch(move |data| match weak.upgrade() {
            Some(terminal) => {
                terminal.feed(data);
                true
            }
            None => false,
        });
        let weak = Rc::downgrade(session);
        terminal.connect_commit(move |_, text, _| {
            if let Some(session) = weak.upgrade() {
                session.write(text.as_bytes());
            }
        });
        // The program learns the terminal's size when it changes.
        let weak = Rc::downgrade(session);
        let last = Cell::new((0, 0));
        terminal.add_tick_callback(move |terminal, _| {
            let Some(session) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let size = (terminal.row_count(), terminal.column_count());
            if size != last.get() && size.0 > 0 && size.1 > 0 {
                last.set(size);
                session.resize(
                    size.0.clamp(1, u16::MAX as _) as u16,
                    size.1.clamp(1, u16::MAX as _) as u16,
                );
            }
            glib::ControlFlow::Continue
        });

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&terminal)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        self.0
            .terminals
            .add_named(&scrolled, Some(&session.id.to_string()));
        self.0
            .terminal_widgets
            .borrow_mut()
            .insert(session.id, terminal.clone());
        terminal
    }

    /// Send `signal` to the focused process.
    fn send(&self, signal: i32) {
        let Some(focus) = self.focused() else { return };
        let Some(id) = focus.target else { return };
        match procs::send(id, signal) {
            Ok(()) => {
                if signal == libc::SIGTERM {
                    self.0.asked.borrow_mut().insert(id);
                }
                self.window().notify(&format!(
                    "Sent {} to {}",
                    procs::signal_name(signal),
                    focus.name
                ));
            }
            Err(e) => self
                .window()
                .error(format!("Could not signal {}: {e}", focus.name)),
        }
        self.refresh();
    }

    /// The stop button: ask the process to quit, and kill it if it was
    /// asked already.
    async fn stop(&self) {
        let Some(focus) = self.focused() else { return };
        let Some(id) = focus.target else { return };
        let name = &focus.name;
        let parent = self.window().widget();
        if self.0.asked.borrow().contains(&id) {
            let body = format!(
                "{name} was already asked to quit. Force stopping ends it instantly with \
                 SIGKILL, without letting it save its work or clean up."
            );
            if ui::confirm(
                &parent,
                &format!("Force Stop {name}?"),
                &body,
                "_Force Stop",
                true,
            )
            .await
            {
                self.send(libc::SIGKILL);
            }
        } else {
            let body = format!(
                "{name} will be asked to quit (SIGTERM), and can save its work first. If it \
                 does not quit, you can then force stop it."
            );
            if ui::confirm(&parent, &format!("Stop {name}?"), &body, "_Stop", true).await {
                self.send(libc::SIGTERM);
            }
        }
    }
}

/// The navigation tag of a bottle's process manager.
pub fn tag(bottle: &str) -> String {
    format!("processes:{bottle}")
}

fn key(entry: &Entry) -> Key {
    match (&entry.process, entry.is_program) {
        (Some(process), false) => Key::Process(entry.session.id, process.id),
        _ => Key::Program(entry.session.id),
    }
}

/// A row's title, subtitle and the text search matches.
fn describe(entry: &Entry) -> (String, String, String) {
    let title = if entry.is_program {
        entry.session.name.clone()
    } else {
        entry
            .process
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default()
    };
    let subtitle = match &entry.process {
        Some(p) => format!("PID {} · {}", p.id.pid, procs::state_label(p.state)),
        None => entry.session.status().describe(),
    };
    let search = match &entry.process {
        Some(p) => format!("{title}\n{}\n{}", p.command, p.id.pid),
        None => title.clone(),
    }
    .to_lowercase();
    (title, subtitle, search)
}

fn signal_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    for section in procs::SIGNALS {
        let part = gio::Menu::new();
        for (signal, name, label) in *section {
            let item = gio::MenuItem::new(Some(&format!("{label} ({name})")), None);
            item.set_action_and_target_value(Some("process.signal"), Some(&signal.to_variant()));
            part.append_item(&item);
        }
        menu.append_section(None, &part);
    }
    menu
}

fn style_terminal(terminal: &vte4::Terminal) {
    let dark = adw::StyleManager::default().is_dark();
    let (fg, bg) = if dark {
        ("#ffffff", "#1e1e1e")
    } else {
        ("#000000", "#ffffff")
    };
    let rgba = |s: &str| gtk::gdk::RGBA::parse(s).unwrap_or(gtk::gdk::RGBA::BLACK);
    terminal.set_colors(Some(&rgba(fg)), Some(&rgba(bg)), &[]);
}

/// The bottle's power button: stop its Rosemary server cleanly, or kill
/// everything Butter started in it.
pub fn power_button(window: &Window, bottle_id: &str) -> gtk::MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some("_Stop"), Some("power.stop"));
    menu.append(Some("_Force Stop"), Some("power.force-stop"));
    let button = gtk::MenuButton::builder()
        .icon_name("system-shutdown-symbolic")
        .tooltip_text("Stop Bottle")
        .menu_model(&menu)
        .build();

    let group = gio::SimpleActionGroup::new();
    let stop = gio::SimpleAction::new("stop", None);
    let (w, id) = (window.clone(), bottle_id.to_string());
    stop.connect_activate(move |action, _| {
        let (window, id, action) = (w.clone(), id.clone(), action.clone());
        glib::spawn_future_local(async move {
            action.set_enabled(false);
            stop_server(&window, &id).await;
            action.set_enabled(true);
        });
    });
    group.add_action(&stop);
    let force = gio::SimpleAction::new("force-stop", None);
    let (w, id) = (window.clone(), bottle_id.to_string());
    let anchor = button.downgrade();
    force.connect_activate(move |_, _| {
        let (window, id, anchor) = (w.clone(), id.clone(), anchor.clone());
        glib::spawn_future_local(async move {
            let Some(anchor) = anchor.upgrade() else {
                return;
            };
            force_stop(&window, &anchor, &id).await;
        });
    });
    group.add_action(&force);
    button.insert_action_group("power", Some(&group));
    button
}

/// Ask the bottle's Rosemary server to shut down, with `rosemary server --stop`.
async fn stop_server(window: &Window, id: &str) {
    let Some(bottle) = bottle::load(id) else {
        return;
    };
    let mut command = match bottle.server_command(&["--stop"]) {
        Ok(command) => command,
        Err(e) => {
            window.error(e);
            return;
        }
    };
    let output = crate::process::background(move || command.output()).await;
    match output {
        Ok(output) if output.status.success() => window.notify(&format!(
            "Asked the Rosemary server in {} to stop",
            bottle.config.name
        )),
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stderr);
            let reason = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| output.status.to_string());
            window.error(format!("Could not stop the Rosemary server: {reason}"));
        }
        Err(e) => window.error(format!("Could not run Rosemary: {e}")),
    }
}

/// Kill every process Butter started in the bottle, after asking.
async fn force_stop(window: &Window, parent: &impl IsA<gtk::Widget>, id: &str) {
    let name = bottle::load(id)
        .map(|b| b.config.name)
        .unwrap_or_else(|| id.to_string());
    if !ui::confirm(
        parent,
        &format!("Force Stop {name}?"),
        "Every program running in this bottle, and everything they started, will be ended at once \
         with SIGKILL. They cannot save their work or clean up, so unsaved changes will be lost.",
        "_Force Stop",
        true,
    )
    .await
    {
        return;
    }
    match window.sessions().force_stop(id).await {
        0 => window.notify("Nothing was running"),
        1 => window.notify("Stopped 1 process"),
        n => window.notify(&format!("Stopped {n} processes")),
    }
}
