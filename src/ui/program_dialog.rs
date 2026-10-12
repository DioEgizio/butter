//! Adding or editing a bottle's program.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::bottle::{self, Backend, Program};
use crate::ui::{self, window::Window};

/// Choose an `.app` bundle (a folder) or a Mach-O executable (a file).
pub async fn choose_target(window: &Window, bundle: bool) -> Option<PathBuf> {
    let dialog = gtk::FileDialog::builder().modal(true).build();
    let parent = window.widget();
    let file = if bundle {
        dialog.set_title("Choose an Application Bundle");
        dialog.select_folder_future(Some(&parent)).await
    } else {
        dialog.set_title("Choose a Mac Executable");
        dialog.open_future(Some(&parent)).await
    };
    file.ok().and_then(|f| f.path())
}

/// Show the program editor. `on_save` receives the result.
pub fn present(window: &Window, existing: Option<Program>, on_save: impl Fn(Program) + 'static) {
    let editing = existing.is_some();
    let path = Rc::new(RefCell::new(existing.as_ref().map(|p| p.path.clone())));

    let name = adw::EntryRow::builder().title("Name").build();
    let location = adw::ActionRow::builder()
        .title("Location")
        .subtitle("None chosen")
        .subtitle_selectable(true)
        .build();
    let choose_app = ui::icon_button("folder-open-symbolic", "Choose App Bundle…");
    let choose_exe = ui::icon_button("application-x-executable-symbolic", "Choose Executable…");
    location.add_suffix(&choose_app);
    location.add_suffix(&choose_exe);
    let args = adw::EntryRow::builder().title("Arguments").build();
    let labels: Vec<&str> = Backend::ALL.iter().map(|b| b.label()).collect();
    let backend = ui::combo_row(
        "Backend",
        Some("Automatic uses the bottle's setting"),
        &labels,
        0,
    );

    if let Some(program) = &existing {
        name.set_text(&program.name);
        location.set_subtitle(&glib::markup_escape_text(
            &program.path.display().to_string(),
        ));
        args.set_text(&join_args(&program.args));
        backend.set_selected(program.backend.index());
    }

    let group = adw::PreferencesGroup::builder()
        .description("An .app bundle, or a Mach-O executable")
        .build();
    group.add(&name);
    group.add(&location);
    group.add(&args);
    group.add(&backend);
    let page = adw::PreferencesPage::new();
    page.add(&group);

    let cancel = gtk::Button::with_mnemonic("_Cancel");
    let save = gtk::Button::with_mnemonic(if editing { "_Save" } else { "_Add" });
    save.add_css_class("suggested-action");
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title(if editing {
            "Edit Program"
        } else {
            "Add Program"
        })
        .content_width(480)
        .content_height(420)
        .child(&toolbar)
        .build();

    let validate = {
        let save = save.clone();
        let name = name.clone();
        let path = path.clone();
        let args = args.clone();
        move || {
            let args_ok = glib::shell_parse_argv(args.text().as_str()).is_ok()
                || args.text().trim().is_empty();
            if args_ok {
                args.remove_css_class("error");
            } else {
                args.add_css_class("error");
            }
            save.set_sensitive(
                path.borrow().is_some() && !name.text().trim().is_empty() && args_ok,
            );
        }
    };
    validate();
    name.connect_changed({
        let validate = validate.clone();
        move |_| validate()
    });
    args.connect_changed({
        let validate = validate.clone();
        move |_| validate()
    });

    for (button, bundle) in [(choose_app, true), (choose_exe, false)] {
        let window = window.clone();
        let path = path.clone();
        let name = name.clone();
        let location = location.clone();
        let validate = validate.clone();
        button.connect_clicked(move |_| {
            let window = window.clone();
            let path = path.clone();
            let name = name.clone();
            let location = location.clone();
            let validate = validate.clone();
            glib::spawn_future_local(async move {
                if let Some(chosen) = choose_target(&window, bundle).await {
                    if name.text().trim().is_empty() {
                        name.set_text(&bottle::program_name(&chosen));
                    }
                    location.set_subtitle(&glib::markup_escape_text(&chosen.display().to_string()));
                    *path.borrow_mut() = Some(chosen);
                    validate();
                }
            });
        });
    }

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    save.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            let Some(path) = path.borrow().clone() else {
                return;
            };
            let text = args.text();
            let args = if text.trim().is_empty() {
                Vec::new()
            } else {
                glib::shell_parse_argv(text.as_str())
                    .map(|v| {
                        v.into_iter()
                            .map(|a| a.to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            on_save(Program {
                name: name.text().trim().to_string(),
                path,
                args,
                backend: Backend::from_index(backend.selected()),
            });
            dialog.close();
        }
    });
    dialog.present(Some(&window.widget()));
}

/// Arguments as one line the user can edit, quoted where needed.
fn join_args(args: &[String]) -> String {
    let plain = |a: &str| {
        !a.is_empty()
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_=.,/:@+%".contains(c))
    };
    args.iter()
        .map(|a| match plain(a) {
            true => a.clone(),
            false => glib::shell_quote(a).to_string_lossy().into_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}
