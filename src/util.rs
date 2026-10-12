//! Small helpers shared by the backend modules.

use std::path::Path;

/// Write `contents` to `path` without leaving a half-written file behind.
pub fn write_atomic(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A directory-safe identifier derived from `name`.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug
        .trim_end_matches('-')
        .trim_start_matches('.')
        .to_string();
    if slug.is_empty() {
        "bottle".into()
    } else {
        slug
    }
}

/// Total size of the files under `path`, not following symlinks.
pub fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => dir_size(&entry.path()),
            Ok(t) if t.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

pub fn format_size(bytes: u64) -> String {
    gtk::glib::format_size(bytes).to_string()
}

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A short local date for a Unix timestamp.
pub fn format_date(timestamp: i64) -> String {
    gtk::glib::DateTime::from_unix_local(timestamp)
        .and_then(|d| d.format("%x"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// A short local date for an ISO 8601 timestamp such as GitHub's.
pub fn format_iso_date(iso: &str) -> String {
    gtk::glib::DateTime::from_iso8601(iso, None)
        .and_then(|d| d.to_local())
        .and_then(|d| d.format("%x"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| iso.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("My Game!"), "my-game");
        assert_eq!(slugify("  ..hidden  "), "hidden");
        assert_eq!(slugify("///"), "bottle");
        assert_eq!(slugify("v0.2.0 test"), "v0.2.0-test");
    }
}
