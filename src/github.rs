//! Rosemary release information from the official GitHub repository.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::config::{self, Paths};

/// How long a fetched release list is reused before asking GitHub again.
/// Unauthenticated API calls are limited to 60 an hour.
const CACHE_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// How long a commit list is trusted before it refreshes by itself. Anything
/// sooner takes the user pressing refresh.
const COMMITS_AUTO_REFRESH: Duration = Duration::from_secs(12 * 60 * 60);

/// How many recent commits are listed.
const COMMITS_SHOWN: usize = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// A file attached to a release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
    /// `sha256:<hex>`, which GitHub computes for each upload.
    #[serde(default)]
    pub digest: Option<String>,
}

impl Asset {
    /// The SHA-256 GitHub reports for this file, as lowercase hex.
    pub fn sha256(&self) -> Option<String> {
        let hex = self.digest.as_deref()?.strip_prefix("sha256:")?;
        (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    }
}

impl Release {
    /// The display title: the release name, or its tag.
    pub fn title(&self) -> &str {
        match self.name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name,
            _ => &self.tag_name,
        }
    }

    /// The Steam compatibility tool archive built for this computer's
    /// architecture, such as `rosemary-steam-tool-0.2.0-x86_64.tar.zst`. It
    /// carries the same self-contained bundle as the AppImage, unpacked.
    /// The smaller `.tar.zst` is preferred over the `.tar.gz` of the same
    /// content, to save on download bandwidth.
    pub fn steam_tool(&self) -> Option<&Asset> {
        let arch = std::env::consts::ARCH;
        ["tar.zst", "tar.gz"].iter().find_map(|ext| {
            let suffix = format!("-{arch}.{ext}");
            self.assets
                .iter()
                .find(|a| a.name.starts_with("rosemary-steam-tool-") && a.name.ends_with(&suffix))
        })
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// The checksum `sha256sum` output lists for `name`, as lowercase hex.
pub fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        // `sha256sum` marks binary-mode lines with `*`.
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// The source archive GitHub generates for a tag.
pub fn source_url(tag: &str) -> String {
    format!(
        "https://github.com/{}/{}/archive/refs/tags/{tag}.tar.gz",
        config::ROSEMARY_OWNER,
        config::ROSEMARY_REPO
    )
}

/// The source archive GitHub generates for a commit.
pub fn commit_source_url(sha: &str) -> String {
    format!(
        "https://github.com/{}/{}/archive/{sha}.tar.gz",
        config::ROSEMARY_OWNER,
        config::ROSEMARY_REPO
    )
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(config::user_agent())
        .timeout_connect(Some(Duration::from_secs(20)))
        .build()
        .into()
}

/// A small text file, such as a release's `SHA256SUMS`.
pub fn fetch_text(url: &str) -> anyhow::Result<String> {
    let mut response = agent()
        .get(url)
        .call()
        .with_context(|| format!("could not download {url}"))?;
    Ok(response.body_mut().read_to_string()?)
}

/// Where a release list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Network,
    Cache,
    /// The network failed and an older list was used instead.
    StaleCache,
}

/// Rosemary's releases, newest first, without drafts.
///
/// A recent cached copy is used unless `refresh` is set; when GitHub cannot
/// be reached, any cached copy is better than nothing.
pub fn releases(refresh: bool) -> anyhow::Result<(Vec<Release>, Origin)> {
    let cache = Paths::get().releases_cache();
    let fresh = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < CACHE_LIFETIME);
    if fresh && !refresh {
        if let Ok(list) = read_cache() {
            return Ok((list, Origin::Cache));
        }
    }
    match fetch() {
        Ok(list) => {
            if let Some(parent) = cache.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = crate::util::write_atomic(&cache, serde_json::to_string(&list)?.as_bytes());
            Ok((list, Origin::Network))
        }
        Err(e) => match read_cache() {
            Ok(list) => Ok((list, Origin::StaleCache)),
            Err(_) => Err(e),
        },
    }
}

fn read_cache() -> anyhow::Result<Vec<Release>> {
    let text = std::fs::read_to_string(Paths::get().releases_cache())?;
    Ok(serde_json::from_str(&text)?)
}

fn fetch() -> anyhow::Result<Vec<Release>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases?per_page=50",
        config::ROSEMARY_OWNER,
        config::ROSEMARY_REPO
    );
    let mut response = agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .with_context(|| format!("could not reach {url}"))?;
    let text = response.body_mut().read_to_string()?;
    parse(&text)
}

/// The release notes (Markdown) of the release tagged `tag`, asked of GitHub
/// directly so a failure means they genuinely could not be fetched.
pub fn release_notes(tag: &str) -> anyhow::Result<String> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/tags/{tag}",
        config::ROSEMARY_OWNER,
        config::ROSEMARY_REPO
    );
    let mut response = agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .with_context(|| format!("could not reach {url}"))?;
    let text = response.body_mut().read_to_string()?;
    let release: Release =
        serde_json::from_str(&text).context("GitHub returned an unexpected release")?;
    Ok(release.body.unwrap_or_default())
}

/// A commit on Rosemary's default branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    /// The first line of the commit message.
    pub summary: String,
    pub author: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub html_url: String,
}

impl Commit {
    pub fn short_sha(&self) -> &str {
        self.sha.get(..7).unwrap_or(&self.sha)
    }
}

/// The recent commits of Rosemary's default branch, newest first.
///
/// Unlike releases, the list is not refetched whenever it is a few minutes
/// old: the cached copy stands for 12 hours unless `refresh` is set. When
/// GitHub cannot be reached, any cached copy is better than nothing.
pub fn commits(refresh: bool) -> anyhow::Result<(Vec<Commit>, Origin)> {
    let cache = Paths::get().commits_cache();
    let fresh = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < COMMITS_AUTO_REFRESH);
    if fresh
        && !refresh
        && let Ok(list) = read_commits_cache()
    {
        return Ok((list, Origin::Cache));
    }
    match fetch_commits() {
        Ok(list) => {
            if let Some(parent) = cache.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = crate::util::write_atomic(&cache, serde_json::to_string(&list)?.as_bytes());
            Ok((list, Origin::Network))
        }
        Err(e) => match read_commits_cache() {
            Ok(list) => Ok((list, Origin::StaleCache)),
            Err(_) => Err(e),
        },
    }
}

/// When the commit list was last fetched, as seconds since the epoch.
pub fn commits_fetched_at() -> Option<i64> {
    let modified = std::fs::metadata(Paths::get().commits_cache())
        .and_then(|m| m.modified())
        .ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(secs).ok()
}

fn read_commits_cache() -> anyhow::Result<Vec<Commit>> {
    let text = std::fs::read_to_string(Paths::get().commits_cache())?;
    Ok(serde_json::from_str(&text)?)
}

fn fetch_commits() -> anyhow::Result<Vec<Commit>> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/commits?per_page={COMMITS_SHOWN}",
        config::ROSEMARY_OWNER,
        config::ROSEMARY_REPO
    );
    let mut response = agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .with_context(|| format!("could not reach {url}"))?;
    let text = response.body_mut().read_to_string()?;
    parse_commits(&text)
}

pub fn parse_commits(json: &str) -> anyhow::Result<Vec<Commit>> {
    #[derive(Deserialize)]
    struct Raw {
        sha: String,
        #[serde(default)]
        html_url: String,
        commit: RawCommit,
    }
    #[derive(Deserialize)]
    struct RawCommit {
        #[serde(default)]
        message: String,
        author: Option<RawAuthor>,
    }
    #[derive(Deserialize)]
    struct RawAuthor {
        #[serde(default)]
        name: String,
        #[serde(default)]
        date: Option<String>,
    }
    let raw: Vec<Raw> =
        serde_json::from_str(json).context("GitHub returned an unexpected commit list")?;
    Ok(raw
        .into_iter()
        .map(|r| {
            let (author, date) = r
                .commit
                .author
                .map(|a| (a.name, a.date))
                .unwrap_or_default();
            Commit {
                sha: r.sha,
                summary: r
                    .commit
                    .message
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                author,
                date,
                html_url: r.html_url,
            }
        })
        .collect())
}

pub fn parse(json: &str) -> anyhow::Result<Vec<Release>> {
    let mut list: Vec<Release> =
        serde_json::from_str(json).context("GitHub returned an unexpected release list")?;
    list.retain(|r| !r.draft);
    list.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_releases() {
        let json = r#"[
            {"tag_name":"v0.1.0","name":"","published_at":"2026-01-01T00:00:00Z","prerelease":false,"draft":false,"html_url":"x"},
            {"tag_name":"v0.3.0-rc1","name":"RC","published_at":"2026-09-01T00:00:00Z","prerelease":true,"draft":false,"html_url":"x"},
            {"tag_name":"v0.2.0","name":"Rosemary 0.2.0","published_at":"2026-06-01T00:00:00Z","prerelease":false,"draft":false,"html_url":"x","extra":1},
            {"tag_name":"v9","draft":true}
        ]"#;
        let list = parse(json).unwrap();
        let tags: Vec<_> = list.iter().map(|r| r.tag_name.as_str()).collect();
        assert_eq!(tags, ["v0.3.0-rc1", "v0.2.0", "v0.1.0"]);
        assert_eq!(list[2].title(), "v0.1.0");
        assert_eq!(list[1].title(), "Rosemary 0.2.0");
        assert_eq!(
            source_url(&list[1].tag_name),
            "https://github.com/microlift/rosemary/archive/refs/tags/v0.2.0.tar.gz"
        );
        assert!(list[1].assets.is_empty());
    }

    #[test]
    fn parses_commits() {
        let json = r#"[
            {"sha":"0123456789abcdef","html_url":"u","extra":1,
             "commit":{"message":"Fix a thing\n\nLong body","author":{"name":"Ann","date":"2026-10-01T00:00:00Z"}}},
            {"sha":"abc","commit":{"message":"","author":null}}
        ]"#;
        let list = parse_commits(json).unwrap();
        assert_eq!(list[0].short_sha(), "0123456");
        assert_eq!(list[0].summary, "Fix a thing");
        assert_eq!(list[0].author, "Ann");
        assert_eq!(list[1].short_sha(), "abc");
        assert_eq!(
            commit_source_url("abc"),
            "https://github.com/microlift/rosemary/archive/abc.tar.gz"
        );
    }

    #[test]
    fn finds_the_steam_tool() {
        let arch = std::env::consts::ARCH;
        let json = format!(
            r#"[{{"tag_name":"v0.2.0","assets":[
                {{"name":"Rosemary-0.2.0-{arch}.AppImage","browser_download_url":"a"}},
                {{"name":"rosemary-steam-tool-0.2.0-other.tar.gz","browser_download_url":"o"}},
                {{"name":"rosemary-steam-tool-0.2.0-{arch}.tar.gz","browser_download_url":"g","size":5}},
                {{"name":"rosemary-steam-tool-0.2.0-{arch}.tar.zst","browser_download_url":"z","size":3,
                  "digest":"sha256:ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789"}}
            ]}}]"#
        );
        let list = parse(&json).unwrap();
        let tool = list[0].steam_tool().unwrap();
        assert_eq!(tool.browser_download_url, "z");
        assert_eq!(
            tool.sha256().as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert!(list[0].asset("SHA256SUMS").is_none());

        // Without a `.tar.zst`, the `.tar.gz` is used.
        let json = format!(
            r#"[{{"tag_name":"v0.2.0","assets":[
                {{"name":"rosemary-steam-tool-0.2.0-{arch}.tar.gz","browser_download_url":"g"}}
            ]}}]"#
        );
        let list = parse(&json).unwrap();
        assert_eq!(list[0].steam_tool().unwrap().browser_download_url, "g");
    }

    #[test]
    fn reads_checksum_lists() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let sums =
            format!("{a}  Rosemary-0.2.0-x86_64.AppImage\n{b} *rosemary-steam-tool.tar.gz\n");
        assert_eq!(
            checksum_for(&sums, "rosemary-steam-tool.tar.gz").as_deref(),
            Some(b.as_str())
        );
        assert_eq!(
            checksum_for(&sums, "Rosemary-0.2.0-x86_64.AppImage").as_deref(),
            Some(a.as_str())
        );
        assert!(checksum_for(&sums, "missing").is_none());
    }
}
