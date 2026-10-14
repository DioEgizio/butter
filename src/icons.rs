//! The icons Butter draws, bundled so they look the same on every desktop.
//!
//! Without these, GTK looks icon names up only in the desktop's own theme
//! (Breeze on KDE, say), which lacks some of them. They are added as a
//! fallback resource path, so a name the theme lacks resolves to the
//! Adwaita icon instead of a missing-image placeholder.

use gtk::{gdk, gio, glib};

const RESOURCE_PATH: &str = "/io/github/microlift/Butter/icons";

pub fn init() {
    let data = glib::Bytes::from_static(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/butter.gresource"
    )));
    match gio::Resource::from_data(&data) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => {
            eprintln!("butter: could not load bundled icons: {e}");
            return;
        }
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path(RESOURCE_PATH);
    }
}
