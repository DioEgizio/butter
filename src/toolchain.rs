//! Finding, and if need be installing, the Rust toolchain runners are built with.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::config::Paths;
use crate::process::Reporter;
use crate::settings::Settings;

/// A usable `cargo`, with the `rustc` beside it.
#[derive(Debug, Clone)]
pub struct Toolchain {
    pub cargo: PathBuf,
    pub rustc: semver::Version,
    pub source: &'static str,
    /// Variables the toolchain needs, such as `RUSTUP_HOME`.
    pub env: Vec<(String, OsString)>,
}

impl Toolchain {
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.cargo);
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        if let Some(bin) = self.cargo.parent() {
            // rustup's proxies and build scripts look for rustc on PATH.
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![bin.to_path_buf()];
            paths.extend(std::env::split_paths(&path));
            if let Ok(joined) = std::env::join_paths(paths) {
                command.env("PATH", joined);
            }
        }
        command
    }

    pub fn satisfies(&self, required: Option<&semver::Version>) -> bool {
        required.is_none_or(|r| version_core(&self.rustc) >= *r)
    }

    pub fn describe(&self) -> String {
        format!("Rust {} ({})", self.rustc, self.source)
    }
}

/// `1.99.0-nightly` counts as 1.99.0 for a minimum-version check.
fn version_core(v: &semver::Version) -> semver::Version {
    semver::Version::new(v.major, v.minor, v.patch)
}

fn managed_env() -> Vec<(String, OsString)> {
    let root = Paths::get().toolchain();
    vec![
        ("RUSTUP_HOME".into(), root.join("rustup").into()),
        ("CARGO_HOME".into(), root.join("cargo").into()),
    ]
}

fn managed_cargo() -> PathBuf {
    Paths::get().toolchain().join("cargo/bin/cargo")
}

pub fn managed_installed() -> bool {
    managed_cargo().is_file()
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Every toolchain Butter can find, best first.
pub fn detect_all(settings: &Settings) -> Vec<Toolchain> {
    type Candidate = (PathBuf, &'static str, Vec<(String, OsString)>);
    let mut candidates: Vec<Candidate> = Vec::new();
    if let Some(path) = settings
        .cargo_override
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        candidates.push((PathBuf::from(path.trim()), "custom", Vec::new()));
    }
    candidates.push((managed_cargo(), "installed by Butter", managed_env()));
    if let Some(path) = find_in_path("cargo") {
        candidates.push((path, "system", Vec::new()));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push((
            PathBuf::from(home).join(".cargo/bin/cargo"),
            "rustup",
            Vec::new(),
        ));
    }
    candidates.push((
        "/usr/lib/sdk/rust-stable/bin/cargo".into(),
        "Flatpak SDK extension",
        Vec::new(),
    ));

    let mut seen = Vec::new();
    let mut found = Vec::new();
    for (cargo, source, env) in candidates {
        let Ok(real) = cargo.canonicalize() else {
            continue;
        };
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        if let Some(rustc) = rustc_version(&cargo, &env) {
            found.push(Toolchain {
                cargo,
                rustc,
                source,
                env,
            });
        }
    }
    found
}

/// The first toolchain new enough for `required`, else the first found.
pub fn best(settings: &Settings, required: Option<&semver::Version>) -> Option<Toolchain> {
    let all = detect_all(settings);
    all.iter()
        .find(|t| t.satisfies(required))
        .or(all.first())
        .cloned()
}

fn rustc_version(cargo: &Path, env: &[(String, OsString)]) -> Option<semver::Version> {
    let sibling = cargo.with_file_name("rustc");
    let rustc = if sibling.is_file() {
        sibling
    } else {
        PathBuf::from("rustc")
    };
    let output = Command::new(rustc)
        .arg("--version")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_rustc_version(&String::from_utf8_lossy(&output.stdout))
}

/// `rustc 1.97.0 (c980f4866 2026-06-30)` -> 1.97.0
pub fn parse_rustc_version(text: &str) -> Option<semver::Version> {
    let word = text.split_whitespace().nth(1)?;
    semver::Version::parse(word).ok()
}

/// A requirement like `1.99` or `1.99.1` as a version.
pub fn parse_requirement(text: &str) -> Option<semver::Version> {
    let mut parts = text.trim().split('.').map(|p| p.parse::<u64>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some(semver::Version::new(major, minor, patch))
}

/// The Rust version a Rosemary source tree asks for: `rust-version` in its
/// manifest, else the one its README names.
pub fn required_by_source(root: &Path) -> Option<semver::Version> {
    if let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml"))
        && let Ok(manifest) = text.parse::<toml::Table>()
    {
        let declared = manifest
            .get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("rust-version"))
            .or_else(|| manifest.get("package").and_then(|p| p.get("rust-version")))
            .and_then(|v| v.as_str());
        if let Some(version) = declared.and_then(parse_requirement) {
            return Some(version);
        }
    }
    let readme = std::fs::read_to_string(root.join("README.md")).ok()?;
    required_by_readme(&readme)
}

/// Finds "requires Rust 1.99 or newer" and similar.
pub fn required_by_readme(text: &str) -> Option<semver::Version> {
    let lower = text.to_lowercase();
    let at = lower.find("requires rust ")? + "requires rust ".len();
    let version: String = lower[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    parse_requirement(version.trim_end_matches('.'))
}

/// A native tool or library a source build needs.
#[derive(Debug, Clone)]
pub struct Requirement {
    pub name: &'static str,
    pub found: bool,
    /// Loaded at run time when present, so a build works without it.
    pub optional: bool,
    pub detail: String,
}

impl Requirement {
    /// Missing, and the build will fail for it.
    pub fn blocking(&self) -> bool {
        !self.found && !self.optional
    }
}

/// The version of the first of `modules` pkg-config knows.
fn module_version(tool: &Path, modules: &[&str]) -> Option<String> {
    modules.iter().find_map(|module| {
        let out = Command::new(tool)
            .args(["--modversion", module])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    })
}

/// Check for the C toolchain and libraries Rosemary links against.
pub fn build_requirements() -> Vec<Requirement> {
    let mut list = Vec::new();
    let cc = find_in_path("cc")
        .or_else(|| find_in_path("gcc"))
        .or_else(|| find_in_path("clang"));
    list.push(Requirement {
        name: "C compiler",
        found: cc.is_some(),
        optional: false,
        detail: cc
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "cc not found".into()),
    });
    let pkg_config = find_in_path("pkg-config").or_else(|| find_in_path("pkgconf"));
    list.push(Requirement {
        name: "pkg-config",
        found: pkg_config.is_some(),
        optional: false,
        detail: pkg_config
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
    });
    // (name, pkg-config modules to try, minimum version, optional)
    let libraries: [(&str, &[&str], Option<&str>, bool); 5] = [
        ("SDL 3", &["sdl3"], Some("3.4.0"), false),
        ("libxkbcommon", &["xkbcommon"], None, false),
        ("libcurl", &["libcurl"], None, false),
        ("zlib", &["zlib"], None, false),
        (
            "WebKitGTK",
            &["webkitgtk-6.0", "webkit2gtk-4.1", "webkit2gtk-4.0"],
            None,
            true,
        ),
    ];
    for (name, modules, minimum, optional) in libraries {
        let version = pkg_config
            .as_ref()
            .and_then(|tool| module_version(tool, modules));
        let new_enough = match (&version, minimum) {
            (Some(v), Some(min)) => {
                matches!((parse_requirement(v), parse_requirement(min)), (Some(v), Some(m)) if v >= m)
            }
            (Some(_), None) => true,
            (None, _) => false,
        };
        let detail = match (&version, minimum) {
            (Some(v), Some(min)) if !new_enough => format!("{v} found, {min} or newer needed"),
            (Some(v), _) => v.clone(),
            (None, _) if optional => "not found; loaded at run time when present".into(),
            (None, _) => "development files not found".into(),
        };
        list.push(Requirement {
            name,
            found: new_enough,
            optional,
            detail,
        });
    }
    list
}

fn host_triple() -> anyhow::Result<&'static str> {
    Ok(match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "freebsd") => "x86_64-unknown-freebsd",
        (arch, os) => bail!("rustup has no installer Butter knows for {arch}-{os}"),
    })
}

/// Install a private stable toolchain with rustup, or update it if present.
pub fn install_managed(reporter: &Reporter) -> anyhow::Result<String> {
    let root = Paths::get().toolchain();
    std::fs::create_dir_all(&root)?;
    if managed_installed() {
        reporter.stage("Updating the Rust toolchain", Some(0.1));
        let rustup = root.join("cargo/bin/rustup");
        reporter.run_checked(
            Command::new(rustup)
                .args(["update", "stable", "--no-self-update"])
                .envs(managed_env()),
        )?;
    } else {
        reporter.stage("Downloading rustup", Some(0.0));
        let url = format!(
            "https://static.rust-lang.org/rustup/dist/{}/rustup-init",
            host_triple()?
        );
        let installer = root.join("rustup-init");
        crate::runner::download(&url, &installer, reporter)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755))?;
        reporter.stage("Installing the stable Rust toolchain", Some(0.3));
        let result = reporter.run_checked(
            Command::new(&installer)
                .args([
                    "-y",
                    "--no-modify-path",
                    "--profile",
                    "minimal",
                    "--default-toolchain",
                    "stable",
                ])
                .envs(managed_env()),
        );
        let _ = std::fs::remove_file(&installer);
        result?;
    }
    let version = rustc_version(&managed_cargo(), &managed_env())
        .context("the toolchain was installed, but rustc does not run")?;
    reporter.progress(1.0);
    Ok(format!("Rust {version} is ready"))
}

pub fn remove_managed() -> anyhow::Result<()> {
    let root = Paths::get().toolchain();
    if root.exists() {
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(
            parse_rustc_version("rustc 1.97.0 (c980f4866 2026-06-30)"),
            Some(semver::Version::new(1, 97, 0))
        );
        let nightly = parse_rustc_version("rustc 1.99.0-nightly (abc 2026-01-01)").unwrap();
        assert_eq!(version_core(&nightly), semver::Version::new(1, 99, 0));
        assert_eq!(
            parse_requirement("1.99"),
            Some(semver::Version::new(1, 99, 0))
        );
        assert_eq!(parse_requirement("x"), None);
    }

    #[test]
    fn reads_readme_requirement() {
        let text = "Rosemary uses the Rust 2024 edition and currently requires Rust 1.99 or newer.";
        assert_eq!(
            required_by_readme(text),
            Some(semver::Version::new(1, 99, 0))
        );
        assert_eq!(required_by_readme("nothing here"), None);
    }

    #[test]
    fn manifest_requirement_wins() {
        let dir = std::env::temp_dir().join(format!("butter-req-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[workspace.package]\nrust-version = \"1.98\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("README.md"), "requires Rust 1.99 or newer").unwrap();
        assert_eq!(
            required_by_source(&dir),
            Some(semver::Version::new(1, 98, 0))
        );
        std::fs::write(dir.join("Cargo.toml"), "[workspace]\n").unwrap();
        assert_eq!(
            required_by_source(&dir),
            Some(semver::Version::new(1, 99, 0))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
