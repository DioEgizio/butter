//! Adding or editing one of a bottle's environment variables.

use adw::prelude::*;

use crate::bottle::{EnvTarget, EnvVar};
use crate::ui::{self, window::Window};

/// What setting a variable for `target` does, for the row's subtitle.
fn explain(target: EnvTarget) -> &'static str {
    match target {
        EnvTarget::Guest => "Only the Mac programs see it",
        EnvTarget::Rosemary => {
            "Configures Rosemary and the host libraries it loads; programs never see it"
        }
        EnvTarget::Both => "Set for Rosemary, and for the Mac programs it runs",
    }
}

/// Show the variable editor. `on_save` receives the result, and the name the
/// variable had before, if it was renamed.
pub fn present(
    window: &Window,
    existing: Option<EnvVar>,
    on_save: impl Fn(EnvVar, Option<String>) + 'static,
) {
    let editing = existing.is_some();
    let old_name = existing.as_ref().map(|v| v.name.clone());

    let name = adw::EntryRow::builder().title("Name").build();
    let labels: Vec<&str> = EnvTarget::ALL.iter().map(|t| t.label()).collect();
    let target = ui::combo_row("Set For", Some(explain(EnvTarget::Guest)), &labels, 0);
    let inherit = adw::SwitchRow::builder()
        .title("Use This Computer's Value")
        .subtitle("Pass on the value Butter itself was started with")
        .build();
    let value = adw::EntryRow::builder().title("Value").build();
    let problem = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .margin_top(12)
        .visible(false)
        .build();
    problem.add_css_class("error");

    if let Some(var) = &existing {
        name.set_text(&var.name);
        target.set_selected(var.target.index());
        inherit.set_active(var.value.is_none());
        value.set_text(var.value.as_deref().unwrap_or_default());
    }

    let group = adw::PreferencesGroup::builder()
        .description(
            "Mac programs start with only the variables set for them, plus \
             HOME, PATH, TERM, LANG and a few other basics",
        )
        .build();
    group.add(&name);
    group.add(&target);
    group.add(&inherit);
    group.add(&value);
    group.add(&problem);
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
            "Edit Variable"
        } else {
            "Add Variable"
        })
        .content_width(480)
        .content_height(440)
        .child(&toolbar)
        .build();

    let read = {
        let (name, target, inherit, value) =
            (name.clone(), target.clone(), inherit.clone(), value.clone());
        let enabled = existing.as_ref().is_none_or(|v| v.enabled);
        move || {
            let target = EnvTarget::from_index(target.selected());
            EnvVar {
                name: name.text().trim().to_string(),
                value: (!(inherit.is_active() && target.guest())).then(|| value.text().to_string()),
                target,
                enabled,
            }
        }
    };

    let validate = {
        let (save, target, inherit, value, problem, read) = (
            save.clone(),
            target.clone(),
            inherit.clone(),
            value.clone(),
            problem.clone(),
            read.clone(),
        );
        move || {
            let chosen = EnvTarget::from_index(target.selected());
            target.set_subtitle(explain(chosen));
            // Rosemary has this computer's whole environment already.
            inherit.set_sensitive(chosen.guest());
            value.set_sensitive(!(inherit.is_active() && chosen.guest()));
            let var = read();
            let issue = (!var.name.is_empty()).then(|| var.problem()).flatten();
            problem.set_label(issue.unwrap_or_default());
            problem.set_visible(issue.is_some());
            save.set_sensitive(var.problem().is_none());
        }
    };
    validate();
    name.connect_changed({
        let validate = validate.clone();
        move |_| validate()
    });
    value.connect_changed({
        let validate = validate.clone();
        move |_| validate()
    });
    inherit.connect_active_notify({
        let validate = validate.clone();
        move |_| validate()
    });
    target.connect_selected_notify({
        let validate = validate.clone();
        move |_| validate()
    });
    // A new variable starts out for whoever its name suggests.
    if !editing {
        let target = target.clone();
        let touched = std::rc::Rc::new(std::cell::Cell::new(false));
        target.connect_selected_notify({
            let touched = touched.clone();
            move |_| touched.set(true)
        });
        name.connect_changed(move |name| {
            if !touched.get() {
                let suggested = EnvTarget::suggested(name.text().trim());
                target.set_selected(suggested.index());
                touched.set(false);
            }
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
            let var = read();
            if var.problem().is_some() {
                return;
            }
            let renamed = old_name.clone().filter(|old| *old != var.name);
            on_save(var, renamed);
            dialog.close();
        }
    });
    dialog.present(Some(&window.widget()));
    name.grab_focus();
}
