use crate::error::{Error, Result};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn xdg_data(sub: &str) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join(sub)
}

pub fn xdg_cache(sub: &str) -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join(sub)
}

/// The shell tests writability with access(2); the workspace base falls back
/// exactly the same way when the configured or sibling directory is read-only.
pub fn is_writable(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(value) => out.push(value.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("/");
                }
            }
            Component::Normal(value) => out.push(value),
        }
    }
    out
}

/// `realpath -m`: resolve what exists, normalize the rest lexically.
pub fn realpath_m(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        return canonical;
    }
    // A missing tail may still carry `.`/`..`; realpath -m normalizes them
    // before it gives up on resolution.
    let absolute = lexical_normalize(&absolute);
    let mut existing = absolute.as_path();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if existing.exists() {
            if let Ok(mut base) = std::fs::canonicalize(existing) {
                for part in suffix.iter().rev() {
                    base.push(part);
                }
                return lexical_normalize(&base);
            }
        }
        let Some(parent) = existing.parent() else {
            break;
        };
        suffix.push(existing.file_name().unwrap_or_default().to_os_string());
        existing = parent;
    }
    absolute
}

/// A single shell word for `herdr pane run`. The pane executes the string with
/// a shell, so quoting must survive `cd` and redirections.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_-./:@%+,".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn remove_with_rip(path: &Path) -> Result<()> {
    let output = Command::new("rip").arg(path).output().map_err(|err| {
        Error::message(format!(
            "rookery: cannot run rip for '{}': {err}",
            path.display()
        ))
    })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(Error::message(format!(
        "rookery: cannot remove '{}': {}",
        path.display(),
        detail.trim()
    )))
}

pub fn read_epoch(path: &Path) -> Option<i64> {
    let body = std::fs::read_to_string(path).ok()?;
    body.lines().next()?.trim().parse().ok()
}

/// `find <dir> -newermt @<epoch>` without forking find. Errors are skipped:
/// an unreadable entry is treated as untouched, exactly as find's silent
/// stderr leaves it.
pub fn touched_since(dir: &Path, epoch: i64) -> bool {
    let Some(threshold) = UNIX_EPOCH.checked_add(Duration::from_secs(epoch.max(0) as u64)) else {
        return false;
    };
    touched_recursive(dir, threshold)
}

fn touched_recursive(path: &Path, threshold: SystemTime) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.modified().is_ok_and(|modified| modified > threshold) {
        return true;
    }
    if !meta.is_dir() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        if touched_recursive(&entry.path(), threshold) {
            return true;
        }
    }
    false
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn sha256_hex(body: &str) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(body.as_bytes());
    hex(&hasher.finalize())
}

/// The shell task alphabet; workspace names and task ids share it.
pub fn valid_task(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `tr '[:upper:]' '[:lower:]' | tr -cs '[:alnum:]' '-' | strip | cut 48`.
pub fn task_slug(value: &str) -> String {
    let lowered = value.to_lowercase();
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in lowered.chars() {
        if character.is_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character);
        } else {
            pending_dash = true;
        }
    }
    let mut slug: String = slug.chars().take(48).collect();
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_the_shell_rules() {
        assert_eq!(task_slug("Renamed seat"), "renamed-seat");
        assert_eq!(task_slug("  --Board task!! "), "board-task");
        assert_eq!(task_slug("a".repeat(60).as_str()).len(), 48);
        assert_eq!(task_slug(""), "");
    }

    #[test]
    fn quoting_keeps_shell_words() {
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn realpath_normalizes_missing_tails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("..").join("b");
        assert_eq!(realpath_m(&path), dir.path().join("b"));
    }
}
