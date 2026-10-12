//! Optional sandbox permissions.
//!
//! The Flatpak asks for as little as it can. Features that need more of the
//! host, such as the Steam bridge, are offered during onboarding: Butter
//! explains what each one needs and prints the `flatpak override` command
//! that grants it. Flatpak gives no way to ask for these at run time, so the
//! user runs the command and restarts Butter.
//!
//! Each step of the onboarding is recorded on its own, with the permissions
//! it asked for. When a step's needs change in a later version, its
//! revision goes up: Butter shows it again and offers to remove what is no
//! longer needed.
//!
//! What the sandbox actually allows is read from `/.flatpak-info`, which
//! Flatpak writes at launch with the user's overrides already applied.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::config::{self, Paths};
use crate::util;

/// One permission, in the form `flatpak override` takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// `--filesystem=PATH[:ro]`. Paths start with `~/`.
    Filesystem { path: String, read_only: bool },
    /// `--share=WHAT`, such as `ipc`.
    Share(String),
}

impl Grant {
    fn filesystem(path: &str) -> Grant {
        Grant::Filesystem {
            path: path.into(),
            read_only: true,
        }
    }

    /// The argument that grants this.
    pub fn arg(&self) -> String {
        match self {
            Grant::Filesystem { path, read_only } => {
                format!("--filesystem={path}{}", if *read_only { ":ro" } else { "" })
            }
            Grant::Share(what) => format!("--share={what}"),
        }
    }

    /// The argument that takes this away again.
    pub fn revoke_arg(&self) -> String {
        match self {
            Grant::Filesystem { path, .. } => format!("--nofilesystem={path}"),
            Grant::Share(what) => format!("--unshare={what}"),
        }
    }

    /// A stable name for records: the grant argument without its dashes.
    pub fn key(&self) -> String {
        self.arg().trim_start_matches("--").to_string()
    }

    pub fn from_key(key: &str) -> Option<Grant> {
        let (kind, value) = key.split_once('=')?;
        match kind {
            "filesystem" => {
                let (path, read_only) = match value.strip_suffix(":ro") {
                    Some(path) => (path, true),
                    None => (value.strip_suffix(":rw").unwrap_or(value), false),
                };
                Some(Grant::Filesystem {
                    path: path.into(),
                    read_only,
                })
            }
            "share" => Some(Grant::Share(value.into())),
            _ => None,
        }
    }
}

/// A feature that needs permissions the Flatpak does not have by default.
/// Each is one step of the onboarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    Steam,
    Ipc,
}

/// What kind of display Butter is on, which decides what is worth asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub x11: bool,
}

impl Feature {
    pub const ALL: [Feature; 2] = [Feature::Steam, Feature::Ipc];

    /// The name the step is recorded under. Never change it.
    pub fn id(self) -> &'static str {
        match self {
            Feature::Steam => "steam",
            Feature::Ipc => "x11-ipc",
        }
    }

    /// Raise this when `grants` changes, so the step is asked again.
    pub fn revision(self) -> u32 {
        match self {
            Feature::Steam => 1,
            Feature::Ipc => 1,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Feature::Steam => "Steam Integration",
            Feature::Ipc => "X11 Shared Memory",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Feature::Steam => "applications-games-symbolic",
            Feature::Ipc => "video-display-symbolic",
        }
    }

    /// What the feature is for, in a sentence or two.
    pub fn description(self) -> &'static str {
        match self {
            Feature::Steam => {
                "Lets games that use Steamworks talk to the Steam client on this computer, \
                 for achievements, cloud saves and multiplayer"
            }
            Feature::Ipc => {
                "Some older X11 setups can't show windows unless Butter shares memory with \
                 the X server. Turn this on if Butter's windows stay blank or fail to open"
            }
        }
    }

    /// What the permissions allow, one line each.
    pub fn needs(self) -> &'static [&'static str] {
        match self {
            Feature::Steam => &[
                "Read-only access to ~/.steam",
                "Read-only access to ~/.local/share/Steam",
            ],
            Feature::Ipc => &["Shared memory with the host (IPC namespace)"],
        }
    }

    /// The comment above this feature's command.
    fn comment(self) -> &'static str {
        match self {
            Feature::Steam => {
                "# Steam Integration: let Rosemary load the Steam client\n\
                 # library (steamclient.so) from your Steam folders.\n\
                 # Read-only."
            }
            Feature::Ipc => {
                "# X11 Shared Memory: share the IPC namespace with the\n\
                 # host, which some older X11 setups need to show windows."
            }
        }
    }

    pub fn grants(self) -> Vec<Grant> {
        match self {
            Feature::Steam => vec![
                Grant::filesystem("~/.steam"),
                Grant::filesystem("~/.local/share/Steam"),
            ],
            Feature::Ipc => vec![Grant::Share("ipc".into())],
        }
    }

    /// Whether the feature matters on this display at all. Wayland needs no
    /// shared memory, so the step is only shown on X11.
    pub fn relevant(self, session: Session) -> bool {
        match self {
            Feature::Steam => true,
            Feature::Ipc => session.x11,
        }
    }
}

/// The permissions a sandbox was started with: the `[Context]` group of
/// `/.flatpak-info`.
#[derive(Debug, Clone, Default)]
pub struct Context {
    shared: Vec<String>,
    filesystems: Vec<String>,
    /// The home directory, to compare `/home/me/x` with `~/x`.
    home: String,
}

impl Context {
    fn has(&self, grant: &Grant) -> bool {
        match grant {
            Grant::Share(what) => self.shared.iter().any(|s| s == what),
            Grant::Filesystem { path, read_only } => self.has_path(path, *read_only),
        }
    }

    fn has_path(&self, path: &str, read_only: bool) -> bool {
        let wanted = self.normalize(path);
        let covers = |entry: &str| {
            entry == "host" || entry == "home" || within(&wanted, &self.normalize(entry))
        };
        let mut allowed = false;
        for entry in &self.filesystems {
            if let Some(denied) = entry.strip_prefix('!') {
                if covers(strip_mode(denied).0) {
                    return false;
                }
                continue;
            }
            let (entry, mode) = strip_mode(entry);
            if covers(entry) && (read_only || mode != Some("ro")) {
                allowed = true;
            }
        }
        allowed
    }

    /// `~/x`, `xdg-data/x` and `/home/me/x` all become `~/x`.
    fn normalize(&self, path: &str) -> String {
        let path = path.trim_end_matches('/');
        if let Some(rest) = path.strip_prefix("xdg-data") {
            return format!("~/.local/share{rest}");
        }
        if !self.home.is_empty()
            && let Some(rest) = path.strip_prefix(self.home.trim_end_matches('/'))
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return format!("~{rest}");
        }
        path.to_string()
    }

    pub fn has_feature(&self, feature: Feature) -> bool {
        feature.grants().iter().all(|g| self.has(g))
    }
}

/// `path` with its `:ro`, `:rw` or `:create` suffix split off.
fn strip_mode(entry: &str) -> (&str, Option<&str>) {
    match entry.rsplit_once(':') {
        Some((path, mode @ ("ro" | "rw" | "create"))) => (path, Some(mode)),
        _ => (entry, None),
    }
}

/// Whether `path` is `dir` or inside it.
fn within(path: &str, dir: &str) -> bool {
    path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The running Flatpak: its id and permissions.
#[derive(Debug, Clone)]
pub struct Sandbox {
    pub app_id: String,
    pub context: Context,
}

impl Sandbox {
    /// Parse `/.flatpak-info`.
    pub fn parse(info: &str, home: &str) -> Sandbox {
        let mut group = "";
        let mut app_id = config::APP_ID.to_string();
        let mut context = Context {
            home: home.into(),
            ..Context::default()
        };
        let list = |value: &str| {
            value
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        };
        for line in info.lines().map(str::trim) {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                group = name;
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (group, key.trim()) {
                ("Application", "name") => app_id = value.trim().to_string(),
                ("Context", "shared") => context.shared = list(value),
                ("Context", "filesystems") => context.filesystems = list(value),
                _ => {}
            }
        }
        Sandbox { app_id, context }
    }

    /// The sandbox Butter runs in, or `None` outside Flatpak. Read once:
    /// permissions only change when Butter is started again.
    pub fn current() -> Option<&'static Sandbox> {
        static SANDBOX: OnceLock<Option<Sandbox>> = OnceLock::new();
        SANDBOX
            .get_or_init(|| {
                if !config::is_flatpak() {
                    return None;
                }
                let info = std::fs::read_to_string("/.flatpak-info").ok()?;
                let home = std::env::var("HOME").unwrap_or_default();
                Some(Sandbox::parse(&info, &home))
            })
            .as_ref()
    }
}

/// Whether `feature` can work now. Outside Flatpak, everything can.
pub fn available(feature: Feature) -> bool {
    Sandbox::current().is_none_or(|s| s.context.has_feature(feature))
}

/// Shell code that grants the features in `grant` and takes away the
/// permissions in `revoke`, with a comment on each command.
pub fn script(app_id: &str, grant: &[Feature], revoke: &[(String, Vec<Grant>)]) -> String {
    let mut blocks = Vec::new();
    let command = |comment: &str, args: Vec<String>| {
        let mut text = format!("{comment}\nflatpak override --user \\\n");
        for arg in args {
            text.push_str(&format!("    {arg} \\\n"));
        }
        text.push_str(&format!("    {app_id}\n"));
        text
    };
    for feature in grant {
        let args = feature.grants().iter().map(Grant::arg).collect();
        blocks.push(command(feature.comment(), args));
    }
    for (why, grants) in revoke {
        let args = grants.iter().map(Grant::revoke_arg).collect();
        blocks.push(command(&format!("# {why}"), args));
    }
    blocks.join("\n")
}

/// Where a step stands, as of the last time Butter looked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Never seen granted since the step was recorded.
    #[default]
    Pending,
    /// Granted at the last launch.
    Granted,
    /// Taken away since, and the user has been told.
    Removed,
}

/// The record of one step the user went through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// The feature's revision when the step was shown.
    pub revision: u32,
    /// When the user answered.
    #[serde(default)]
    pub answered: i64,
    /// Whether they wanted the feature.
    pub wanted: bool,
    /// The permissions the step asked for then, as `Grant::key`s.
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub status: Status,
}

/// Every step answered so far, by feature id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub steps: BTreeMap<String, Step>,
}

/// What to tell the user at launch.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Review {
    /// Steps never shown, or shown in an older revision.
    pub ask: Vec<Feature>,
    /// Wanted, but never granted: the command was probably not run.
    pub forgotten: Vec<Feature>,
    /// Granted before, gone now.
    pub removed: Vec<Feature>,
}

impl State {
    pub fn load() -> State {
        std::fs::read_to_string(Paths::get().permissions_file())
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Paths::get().permissions_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        util::write_atomic(&path, toml::to_string_pretty(self)?.as_bytes())
    }

    pub fn wanted(&self, feature: Feature) -> bool {
        self.steps.get(feature.id()).is_some_and(|s| s.wanted)
    }

    /// Compare what the sandbox allows with what was granted last time, and
    /// update each step's status. Features that don't matter on this
    /// display are left alone until they do.
    pub fn review(&mut self, context: &Context, session: Session) -> Review {
        let mut review = Review::default();
        for feature in Feature::ALL {
            if !feature.relevant(session) {
                continue;
            }
            let Some(step) = self.steps.get_mut(feature.id()) else {
                review.ask.push(feature);
                continue;
            };
            if step.revision < feature.revision() {
                review.ask.push(feature);
                continue;
            }
            let granted = context.has_feature(feature);
            match (step.status, granted) {
                (_, true) => step.status = Status::Granted,
                (Status::Granted, false) => {
                    step.status = Status::Removed;
                    review.removed.push(feature);
                }
                (Status::Pending, false) if step.wanted => review.forgotten.push(feature),
                _ => {}
            }
        }
        review
    }

    /// Record the user's answer to `feature`'s step.
    pub fn answer(&mut self, feature: Feature, wanted: bool, context: &Context) {
        let granted = context.has_feature(feature);
        // Declining is deliberate: when the permissions go, nobody needs
        // telling. Should they still be there next launch, they become
        // `Granted` again, and losing them later is worth a word.
        let status = match (wanted, granted) {
            (false, _) => Status::Removed,
            (true, true) => Status::Granted,
            (true, false) => Status::Pending,
        };
        self.steps.insert(
            feature.id().into(),
            Step {
                revision: feature.revision(),
                answered: util::now(),
                wanted,
                permissions: feature.grants().iter().map(Grant::key).collect(),
                status,
            },
        );
    }

    /// Permissions an older revision of `feature`'s step asked for that it
    /// no longer needs, and that the sandbox still has.
    pub fn obsolete(&self, feature: Feature, context: &Context) -> Vec<Grant> {
        let Some(step) = self.steps.get(feature.id()) else {
            return Vec::new();
        };
        let current = feature.grants();
        step.permissions
            .iter()
            .filter_map(|key| Grant::from_key(key))
            .filter(|g| !current.contains(g) && context.has(g))
            .collect()
    }
}

/// What the end of the onboarding should print, given the answers.
pub struct Plan {
    pub grant: Vec<Feature>,
    pub revoke: Vec<(String, Vec<Grant>)>,
}

impl Plan {
    pub fn new(state: &State, context: &Context, answers: &[(Feature, bool)]) -> Plan {
        let mut plan = Plan {
            grant: Vec::new(),
            revoke: Vec::new(),
        };
        for &(feature, wanted) in answers {
            let granted = context.has_feature(feature);
            if wanted && !granted {
                plan.grant.push(feature);
            }
            if !wanted {
                let held: Vec<Grant> = feature
                    .grants()
                    .into_iter()
                    .filter(|g| context.has(g))
                    .collect();
                if !held.is_empty() {
                    plan.revoke
                        .push((format!("{}: no longer wanted.", feature.title()), held));
                }
            }
            let obsolete = state.obsolete(feature, context);
            if !obsolete.is_empty() {
                plan.revoke.push((
                    format!("{}: Butter no longer needs these.", feature.title()),
                    obsolete,
                ));
            }
        }
        plan
    }

    pub fn is_empty(&self) -> bool {
        self.grant.is_empty() && self.revoke.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const X11: Session = Session { x11: true };
    const WAYLAND: Session = Session { x11: false };

    fn context(shared: &str, filesystems: &str) -> Context {
        let info = format!(
            "[Application]\nname=org.example.Butter\n\n[Context]\nshared={shared}\nfilesystems={filesystems}\n"
        );
        Sandbox::parse(&info, "/home/me").context
    }

    #[test]
    fn parses_flatpak_info() {
        let sandbox = Sandbox::parse(
            "[Application]\nname=org.example.Butter\nruntime=x\n\n[Context]\nshared=network;ipc;\nsockets=wayland;\n",
            "/home/me",
        );
        assert_eq!(sandbox.app_id, "org.example.Butter");
        assert!(sandbox.context.has_feature(Feature::Ipc));
        assert!(!sandbox.context.has_feature(Feature::Steam));
    }

    #[test]
    fn steam_needs_both_folders() {
        assert!(!context("network;", "~/.steam:ro;").has_feature(Feature::Steam));
        assert!(
            context("network;", "~/.steam:ro;~/.local/share/Steam:ro;").has_feature(Feature::Steam)
        );
        // Spelled differently, or covered by something broader.
        assert!(context("", "/home/me/.steam;xdg-data/Steam:rw;").has_feature(Feature::Steam));
        assert!(context("", "home:ro;").has_feature(Feature::Steam));
        assert!(context("", "host;").has_feature(Feature::Steam));
        assert!(!context("", "home;!~/.steam;").has_feature(Feature::Steam));
        assert!(!context("", "~/.steamy;~/.local/share/Steam;").has_feature(Feature::Steam));
    }

    #[test]
    fn grant_keys_round_trip() {
        for feature in Feature::ALL {
            for grant in feature.grants() {
                assert_eq!(Grant::from_key(&grant.key()), Some(grant));
            }
        }
    }

    #[test]
    fn first_launch_asks_what_matters_here() {
        let mut state = State::default();
        let review = state.review(&context("network;", ""), WAYLAND);
        assert_eq!(review.ask, [Feature::Steam]);
        let review = state.review(&context("network;", ""), X11);
        assert_eq!(review.ask, [Feature::Steam, Feature::Ipc]);
    }

    #[test]
    fn switching_to_x11_asks_about_ipc() {
        let none = context("network;", "");
        let mut state = State::default();
        state.answer(Feature::Steam, false, &none);
        assert_eq!(state.review(&none, WAYLAND), Review::default());
        assert_eq!(state.review(&none, X11).ask, [Feature::Ipc]);
    }

    #[test]
    fn reminds_until_granted_then_warns_once_when_removed() {
        let none = context("network;", "");
        let steam = context("network;", "~/.steam:ro;~/.local/share/Steam:ro");
        let mut state = State::default();
        state.answer(Feature::Steam, true, &none);

        // The command was not run.
        assert_eq!(state.review(&none, WAYLAND).forgotten, [Feature::Steam]);
        assert_eq!(state.review(&none, WAYLAND).forgotten, [Feature::Steam]);

        // Now it was.
        assert_eq!(state.review(&steam, WAYLAND), Review::default());

        // Taken away: said once.
        assert_eq!(state.review(&none, WAYLAND).removed, [Feature::Steam]);
        assert_eq!(state.review(&none, WAYLAND), Review::default());

        // Granted and removed again: said again.
        assert_eq!(state.review(&steam, WAYLAND), Review::default());
        assert_eq!(state.review(&none, WAYLAND).removed, [Feature::Steam]);
    }

    #[test]
    fn declined_features_are_not_nagged() {
        let none = context("network;", "");
        let mut state = State::default();
        state.answer(Feature::Steam, false, &none);
        assert_eq!(state.review(&none, WAYLAND), Review::default());
    }

    #[test]
    fn deliberate_revokes_are_not_reported() {
        let ipc = context("network;ipc;", "");
        let none = context("network;", "");
        let mut state = State::default();
        state.answer(Feature::Steam, false, &none);
        state.answer(Feature::Ipc, true, &ipc);
        state.answer(Feature::Ipc, false, &ipc);
        assert_eq!(state.review(&none, X11), Review::default());
    }

    #[test]
    fn irrelevant_features_wait_for_their_display() {
        let ipc = context("network;ipc;", "");
        let none = context("network;", "");
        let mut state = State::default();
        state.answer(Feature::Steam, false, &none);
        state.answer(Feature::Ipc, true, &ipc);
        // Removed while on Wayland: said on the next X11 launch.
        assert_eq!(state.review(&none, WAYLAND), Review::default());
        assert_eq!(state.review(&none, X11).removed, [Feature::Ipc]);
    }

    #[test]
    fn newer_revisions_ask_again_and_offer_removals() {
        let held = context("network;", "~/.steam:ro;~/.local/share/Steam:ro;~/.old:ro");
        let mut state = State::default();
        state.answer(Feature::Steam, true, &held);
        let step = state.steps.get_mut("steam").unwrap();
        step.revision = 0;
        step.permissions.push("filesystem=~/.old:ro".into());

        assert_eq!(state.review(&held, WAYLAND).ask, [Feature::Steam]);
        let obsolete = state.obsolete(Feature::Steam, &held);
        assert_eq!(obsolete, [Grant::filesystem("~/.old")]);
        let plan = Plan::new(&state, &held, &[(Feature::Steam, true)]);
        assert!(plan.grant.is_empty());
        assert_eq!(plan.revoke.len(), 1);
    }

    #[test]
    fn scripts_grant_and_revoke() {
        let ipc = context("network;ipc;", "");
        let plan = Plan::new(
            &State::default(),
            &ipc,
            &[(Feature::Steam, true), (Feature::Ipc, false)],
        );
        let text = script("org.example.Butter", &plan.grant, &plan.revoke);
        assert!(text.contains(
            "flatpak override --user \\\n    --filesystem=~/.steam:ro \\\n    \
             --filesystem=~/.local/share/Steam:ro \\\n    org.example.Butter\n"
        ));
        assert!(text.contains("--unshare=ipc \\\n    org.example.Butter\n"));
        assert!(text.lines().filter(|l| l.starts_with('#')).count() >= 3);
    }
}
