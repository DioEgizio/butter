//! Compiles the icons Butter bundles into a GResource.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed=data/butter.gresource.xml");
    println!("cargo:rerun-if-changed=data/icons/bundled");
    let status = Command::new("glib-compile-resources")
        .args(["--sourcedir=data/icons/bundled"])
        .arg(format!("--target={}", out.join("butter.gresource").display()))
        .arg("data/butter.gresource.xml")
        .status()
        .expect("glib-compile-resources not found; install the GLib development tools");
    assert!(status.success(), "glib-compile-resources failed");
}
