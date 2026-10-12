//! Bottles: a Rosemary prefix, the runner it uses, and how to run its programs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::config::Paths;
use crate::permissions::{self, Feature};
use crate::runner::{self, Runner};
use crate::util;

const MANIFEST: &str = "bottle.toml";

/// Generates the small enums the settings pages pick from: each has its
/// variants in order, a label, and the value Rosemary is given.
macro_rules! choice {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => ($label:literal, $value:expr)),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "kebab-case")]
        pub enum $name { #[default] $($variant),+ }

        #[allow(dead_code)]
        impl $name {
            pub const ALL: &[$name] = &[$($name::$variant),+];
            pub fn label(self) -> &'static str {
                match self { $($name::$variant => $label),+ }
            }
            pub fn value(self) -> Option<&'static str> {
                match self { $($name::$variant => $value),+ }
            }
            pub fn index(self) -> u32 {
                Self::ALL.iter().position(|v| *v == self).unwrap_or(0) as u32
            }
            pub fn from_index(i: u32) -> Self {
                Self::ALL.get(i as usize).copied().unwrap_or_default()
            }
        }
    };
}

choice!(
    /// Which execution backend runs a program.
    Backend {
        Auto => ("Automatic", None),
        Native => ("Native", Some("--native")),
        Emulated => ("Emulated (x86-64)", Some("--emulated")),
    }
);

choice!(
    /// The guest architecture of a prefix's Darwin base.
    Arch {
        Host => ("Automatic", None),
        X86_64 => ("x86-64", Some("x86_64")),
        Aarch64 => ("ARM64", Some("aarch64")),
    }
);

choice!(
    /// What happens when a guest writes a file the Darwin base owns.
    SystemFiles {
        Repair => ("Restore at next start", Some("repair")),
        Warn => ("Allow, and warn", Some("warn")),
        Immutable => ("Read-only", Some("immutable")),
    }
);

choice!(
    /// `ROSEMARY_DECORATIONS`
    Decorations {
        Client => ("Mac-style titlebar", None),
        Server => ("Desktop titlebar", Some("server")),
    }
);

choice!(
    /// `ROSEMARY_MENU`
    Menu {
        Default => ("Default", None),
        Titlebar => ("In the titlebar", Some("titlebar")),
        Strip => ("Below the titlebar", Some("strip")),
        Hidden => ("Hidden", Some("none")),
    }
);

choice!(
    /// `ROSEMARY_SWAP_INTERVAL`
    SwapInterval {
        Default => ("Application's choice", None),
        Immediate => ("Off (unlocked)", Some("0")),
        Vsync => ("VSync", Some("1")),
        Adaptive => ("Adaptive VSync", Some("-1")),
    }
);

choice!(
    /// `ROSEMARY_PREFIX_HOME`
    PrefixHome {
        Prefix => ("Isolated (Recommended)", None),
        Host => ("Host home folder", Some("host")),
    }
);

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Program {
    pub name: String,
    /// An `.app` bundle or a Mach-O executable.
    pub path: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub backend: Backend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub arch: Arch,
    #[serde(default)]
    pub system_files: SystemFiles,
    #[serde(default)]
    pub backend: Backend,
    #[serde(default)]
    pub decorations: Decorations,
    #[serde(default)]
    pub menu: Menu,
    #[serde(default)]
    pub swap_interval: SwapInterval,
    #[serde(default)]
    pub prefix_home: PrefixHome,
    #[serde(default = "yes")]
    pub steam: bool,
    #[serde(default = "yes")]
    pub menu_ctrl: bool,
    #[serde(default)]
    pub confine_pointer: bool,
    /// Hide the Vulkan driver from Rosemary (`VK_DRIVER_FILES=/dev/null`).
    #[serde(default)]
    pub disable_metal: bool,
    #[serde(default = "yes")]
    pub process_server: bool,
    #[serde(default)]
    pub dead_flags: bool,
    #[serde(default)]
    pub call_return: bool,
    #[serde(default)]
    pub strict_imports: bool,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub programs: Vec<Program>,
}

impl Config {
    pub fn new(name: &str, runner: Option<String>) -> Config {
        // Round-trips through serde so the defaults live in one place.
        let mut config: Config = toml::from_str(&format!("name = {name:?}")).expect("defaults");
        config.runner = runner;
        config.created = util::now();
        config
    }
}

#[derive(Debug, Clone)]
pub struct Bottle {
    pub id: String,
    pub dir: PathBuf,
    pub config: Config,
}

impl Bottle {
    /// The Rosemary prefix: the directory whose `root/` is the guest's `/`.
    pub fn prefix(&self) -> PathBuf {
        self.dir.join("prefix")
    }

    /// The guest's `/`.
    pub fn drive(&self) -> PathBuf {
        self.prefix().join("root")
    }

    pub fn logs(&self) -> PathBuf {
        self.dir.join("logs")
    }

    pub fn last_log(&self) -> PathBuf {
        self.logs().join("last-run.log")
    }

    /// Records of the programs running here, so a restarted Butter finds them.
    pub fn records(&self) -> PathBuf {
        self.dir.join("run")
    }

    /// Whether Rosemary has made the prefix.
    pub fn is_ready(&self) -> bool {
        self.prefix().join(".rosemary").is_dir()
    }

    pub fn runner(&self) -> Option<Runner> {
        self.config.runner.as_deref().and_then(runner::find)
    }

    fn require_runner(&self) -> anyhow::Result<Runner> {
        let Some(id) = &self.config.runner else {
            bail!(
                "{} has no runner; choose one in its settings",
                self.config.name
            );
        };
        let runner = runner::find(id).with_context(|| format!("runner {id} is not installed"))?;
        if !runner.is_usable() {
            bail!(
                "{} has no executable at {}",
                runner.info.name,
                runner.executable().display()
            );
        }
        Ok(runner)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        util::write_atomic(
            &self.dir.join(MANIFEST),
            toml::to_string_pretty(&self.config)?.as_bytes(),
        )
    }

    /// The environment Rosemary is run with for this bottle.
    pub fn environment(&self) -> Vec<(String, String)> {
        let c = &self.config;
        let mut env = vec![(
            "ROSEMARY_PREFIX".to_string(),
            self.prefix().display().to_string(),
        )];
        let mut set = |key: &str, value: Option<&str>| {
            if let Some(value) = value {
                env.push((key.to_string(), value.to_string()));
            }
        };
        set("ROSEMARY_DECORATIONS", c.decorations.value());
        set("ROSEMARY_MENU", c.menu.value());
        set("ROSEMARY_SWAP_INTERVAL", c.swap_interval.value());
        set("ROSEMARY_PREFIX_HOME", c.prefix_home.value());
        // Without access to Steam's folders the bridge has nothing to load.
        let steam = c.steam && permissions::available(Feature::Steam);
        set("ROSEMARY_STEAM", (!steam).then_some("0"));
        set("ROSEMARY_MENU_CTRL", (!c.menu_ctrl).then_some("0"));
        set("ROSEMARY_CONFINE_POINTER", c.confine_pointer.then_some("1"));
        set("VK_DRIVER_FILES", c.disable_metal.then_some("/dev/null"));
        set("ROSEMARY_SERVER",(!c.process_server).then_some("off"));
        set("ROSEMARY_DEAD_FLAGS", c.dead_flags.then_some("1"));
        set("ROSEMARY_CALL_RETURN", c.call_return.then_some("1"));
        set("ROSEMARY_STRICT_IMPORTS", c.strict_imports.then_some("1"));
        // The user's own variables win over everything above.
        for (key, value) in &c.env {
            env.retain(|(k, _)| k != key);
            env.push((key.clone(), value.clone()));
        }
        env
    }

    /// `rosemary prefix ACTION PREFIX ARGS...` for this bottle.
    ///
    /// Downloads of the Darwin base are allowed: Butter asks before running
    /// anything that may fetch one.
    pub fn prefix_command(&self, action: &str, args: &[&str]) -> anyhow::Result<Command> {
        let runner = self.require_runner()?;
        let mut command = runner.command();
        command
            .arg("prefix")
            .arg(action)
            .arg(self.prefix())
            .args(args)
            .env("ROSEMARY_PREFIX_SETUP", "download");
        Ok(command)
    }

    /// The arguments `rosemary prefix create` needs for this bottle's settings.
    pub fn create_args(&self, with_base: bool) -> Vec<&'static str> {
        if !with_base {
            return vec!["--no-base"];
        }
        let mut args = Vec::new();
        if let Some(arch) = self.config.arch.value() {
            args.extend(["--arch", arch]);
        }
        if let Some(policy) = self.config.system_files.value() {
            args.extend(["--system-files", policy]);
        }
        args
    }

    /// `rosemary run` for `target`, with this bottle's settings. It is
    /// started by [`crate::sessions::Sessions::launch`].
    pub fn run_command(
        &self,
        target: &Path,
        args: &[String],
        backend: Backend,
    ) -> anyhow::Result<Command> {
        let runner = self.require_runner()?;
        let backend = if backend == Backend::Auto {
            self.config.backend
        } else {
            backend
        };
        let mut command = runner.command();
        command.arg("run");
        if let Some(flag) = backend.value() {
            command.arg(flag);
        }
        command
            .arg(target)
            .args(args)
            .envs(self.environment())
            .current_dir(target.parent().unwrap_or(Path::new("/")));
        Ok(command)
    }

    /// `rosemary server ARGS...` for this bottle's prefix.
    pub fn server_command(&self, args: &[&str]) -> anyhow::Result<Command> {
        let mut command = self.require_runner()?.command();
        command
            .arg("server")
            .args(args)
            .envs(self.environment())
            .stdin(Stdio::null());
        Ok(command)
    }
}

pub fn list() -> Vec<Bottle> {
    let Ok(entries) = std::fs::read_dir(Paths::get().bottles()) else {
        return Vec::new();
    };
    let mut list: Vec<Bottle> = entries
        .flatten()
        .filter_map(|e| load_dir(&e.path()))
        .collect();
    list.sort_by_key(|b| b.config.name.to_lowercase());
    list
}

fn load_dir(dir: &Path) -> Option<Bottle> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let config = toml::from_str(&text)
        .map_err(|e| eprintln!("butter: skipping {}: {e}", dir.join(MANIFEST).display()))
        .ok()?;
    Some(Bottle {
        id: dir.file_name()?.to_string_lossy().into_owned(),
        dir: dir.to_path_buf(),
        config,
    })
}

pub fn load(id: &str) -> Option<Bottle> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    load_dir(&Paths::get().bottles().join(id))
}

/// Make a bottle's directory and manifest. Its prefix is created separately,
/// with [`Bottle::prefix_command`], so the output can be shown.
pub fn create(config: Config) -> anyhow::Result<Bottle> {
    let name = config.name.trim();
    if name.is_empty() {
        bail!("a bottle needs a name");
    }
    if list()
        .iter()
        .any(|b| b.config.name.eq_ignore_ascii_case(name))
    {
        bail!("there is already a bottle named {name}");
    }
    let base = util::slugify(name);
    let root = Paths::get().bottles();
    let mut id = base.clone();
    let mut n = 2;
    while root.join(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let bottle = Bottle {
        dir: root.join(&id),
        id,
        config: Config {
            name: name.to_string(),
            ..config
        },
    };
    bottle.save()?;
    Ok(bottle)
}

pub fn delete(bottle: &Bottle) -> anyhow::Result<()> {
    // Only ever a directory Butter made: one holding our manifest.
    if !bottle.dir.join(MANIFEST).is_file() {
        bail!("{} is not a bottle", bottle.dir.display());
    }
    std::fs::remove_dir_all(&bottle.dir)?;
    Ok(())
}

/// Bottles using the runner `id`.
pub fn using_runner(id: &str) -> Vec<Bottle> {
    list()
        .into_iter()
        .filter(|b| b.config.runner.as_deref() == Some(id))
        .collect()
}

pub fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A program's display name from its path: `Foo.app` -> `Foo`.
pub fn program_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".app")
        .map(str::to_string)
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bottle(config: Config) -> Bottle {
        Bottle {
            id: "t".into(),
            dir: "/b/t".into(),
            config,
        }
    }

    #[test]
    fn defaults_survive_a_minimal_manifest() {
        let config = Config::new("Games", Some("rosemary-v0.2.0".into()));
        assert!(config.steam && config.menu_ctrl && config.process_server);
        assert!(!config.dead_flags);
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.runner.as_deref(), Some("rosemary-v0.2.0"));
    }

    #[test]
    fn environment_reflects_settings() {
        let mut config = Config::new("x", None);
        let env = bottle(config.clone()).environment();
        assert_eq!(
            env,
            vec![("ROSEMARY_PREFIX".to_string(), "/b/t/prefix".to_string())]
        );

        config.decorations = Decorations::Server;
        config.steam = false;
        config.swap_interval = SwapInterval::Adaptive;
        config.dead_flags = true;
        config.disable_metal = true;
        config.env.insert("ROSEMARY_STEAM".into(), "1".into());
        config.env.insert("FOO".into(), "bar".into());
        let env: BTreeMap<_, _> = bottle(config).environment().into_iter().collect();
        assert_eq!(env["ROSEMARY_DECORATIONS"], "server");
        assert_eq!(env["ROSEMARY_SWAP_INTERVAL"], "-1");
        assert_eq!(env["ROSEMARY_DEAD_FLAGS"], "1");
        assert_eq!(env["VK_DRIVER_FILES"], "/dev/null");
        assert_eq!(env["ROSEMARY_STEAM"], "1", "user variables win");
        assert_eq!(env["FOO"], "bar");
    }

    #[test]
    fn create_arguments() {
        let mut config = Config::new("x", None);
        assert_eq!(
            bottle(config.clone()).create_args(true),
            ["--system-files", "repair"]
        );
        config.arch = Arch::X86_64;
        config.system_files = SystemFiles::Immutable;
        assert_eq!(
            bottle(config.clone()).create_args(true),
            ["--arch", "x86_64", "--system-files", "immutable"]
        );
        assert_eq!(bottle(config).create_args(false), ["--no-base"]);
    }

    #[test]
    fn names() {
        assert!(valid_env_name("ROSEMARY_OBJC"));
        assert!(!valid_env_name("1X"));
        assert!(!valid_env_name("A-B"));
        assert_eq!(program_name(Path::new("/x/Foo Bar.app")), "Foo Bar");
        assert_eq!(program_name(Path::new("/x/tool")), "tool");
    }
}
