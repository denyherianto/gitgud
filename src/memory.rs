use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::git::GitRepo;

const MEMORY_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const SCAN_DEPTH: usize = 50;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoMemory {
    /// Unix timestamp (seconds) when this record was built
    pub built_at: u64,
    /// Detected commit style: "conventional" or "standard"
    pub commit_style_hint: String,
    /// Conventional commit types seen in history, frequency-ordered, max 10
    pub observed_types: Vec<String>,
    /// Conventional commit scopes seen in history, frequency-ordered, max 15
    pub observed_scopes: Vec<String>,
    /// Branch name prefixes seen (e.g. "feat/", "fix/"), max 10
    pub branch_prefixes: Vec<String>,
    /// Top-level directories present in >30% of commits
    pub risky_paths: Vec<String>,
}

/// Turn a repo root path into a human-readable filename-safe slug.
/// Uses the last two path segments plus a djb2 hash suffix to avoid collisions.
pub fn path_to_slug(root: &Path) -> String {
    let segments: Vec<&str> = root
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    let meaningful = if segments.len() >= 2 {
        format!(
            "{}__{}",
            segments[segments.len() - 2],
            segments[segments.len() - 1]
        )
    } else {
        segments.last().copied().unwrap_or("unknown").to_string()
    };

    let slug: String = meaningful
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // djb2-style hash of the full path for collision resistance
    let mut hash: u64 = 5381;
    for byte in root.to_string_lossy().bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }

    format!("{slug}__{:08x}", hash & 0xffff_ffff)
}

pub fn memory_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("unable to determine user config directory")?
        .join("gitgud")
        .join("repos");
    Ok(dir)
}

pub fn memory_path(root: &Path) -> Result<PathBuf> {
    Ok(memory_dir()?.join(format!("{}.toml", path_to_slug(root))))
}

fn is_fresh(memory: &RepoMemory) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(memory.built_at) < MEMORY_TTL_SECS
}

fn load_memory(path: &Path) -> Option<RepoMemory> {
    let raw = fs::read_to_string(path).ok()?;
    toml::from_str(&raw).ok()
}

fn save_memory(path: &Path, memory: &RepoMemory) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create memory dir {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(memory).context("failed to serialize repo memory")?;
    fs::write(path, raw)
        .with_context(|| format!("failed to write repo memory {}", path.display()))?;
    Ok(())
}

/// Parse a conventional commit subject line: `type(scope): description` or `type: description`.
/// Returns `(type, Option<scope>)` or `None` if not conventional.
fn parse_conventional_subject(subject: &str) -> Option<(String, Option<String>)> {
    let (head, _rest) = subject.split_once(':')?;
    // Strip breaking change marker
    let head = head.trim_end_matches('!');

    if let Some((commit_type, scope_part)) = head.split_once('(') {
        let scope = scope_part.trim_end_matches(')');
        if !commit_type.is_empty()
            && !scope.is_empty()
            && commit_type
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-')
        {
            return Some((commit_type.to_string(), Some(scope.to_string())));
        }
    }

    // No scope
    if !head.is_empty() && head.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        return Some((head.to_string(), None));
    }

    None
}

fn freq_sorted(map: HashMap<String, usize>) -> Vec<String> {
    let mut pairs: Vec<_> = map.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    pairs.into_iter().map(|(k, _)| k).collect()
}

fn detect_risky_paths(repo: &GitRepo) -> Vec<String> {
    let output = match repo.name_only_log(SCAN_DEPTH) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    let mut dir_freq: HashMap<String, usize> = HashMap::new();
    let mut total_commits = 0usize;

    for record in output.split('\u{1e}') {
        let trimmed = record.trim();
        if trimmed.is_empty() {
            continue;
        }
        total_commits += 1;
        let mut seen_in_commit = std::collections::HashSet::new();
        let mut past_blank = false;

        for line in trimmed.lines() {
            if line.trim().is_empty() {
                past_blank = true;
                continue;
            }
            if !past_blank {
                continue;
            }
            // Extract top-level directory only
            let dir = match line.trim().split_once('/') {
                Some((d, _)) => d.to_string(),
                None => continue,
            };
            if seen_in_commit.insert(dir.clone()) {
                *dir_freq.entry(dir).or_default() += 1;
            }
        }
    }

    if total_commits == 0 {
        return Vec::new();
    }

    let threshold = (total_commits * 30 / 100).max(1);
    let mut risky: Vec<_> = dir_freq
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .collect();
    risky.sort_by(|a, b| b.1.cmp(&a.1));
    risky.into_iter().map(|(dir, _)| dir).collect()
}

pub fn build_memory(repo: &GitRepo) -> Result<RepoMemory> {
    let commits = repo.recent_commits(SCAN_DEPTH).unwrap_or_default();
    let branches = repo.local_branches().unwrap_or_default();

    // Commit style: conventional if >50% of commits parse as conventional
    let conventional_count = commits
        .iter()
        .filter(|c| parse_conventional_subject(&c.subject).is_some())
        .count();
    let commit_style_hint = if !commits.is_empty() && conventional_count * 2 >= commits.len() {
        "conventional".to_string()
    } else {
        "standard".to_string()
    };

    // Type and scope frequency
    let mut type_freq: HashMap<String, usize> = HashMap::new();
    let mut scope_freq: HashMap<String, usize> = HashMap::new();
    for commit in &commits {
        if let Some((t, s)) = parse_conventional_subject(&commit.subject) {
            *type_freq.entry(t).or_default() += 1;
            if let Some(scope) = s {
                *scope_freq.entry(scope).or_default() += 1;
            }
        }
    }
    let mut observed_types = freq_sorted(type_freq);
    observed_types.truncate(10);
    let mut observed_scopes = freq_sorted(scope_freq);
    observed_scopes.truncate(15);

    // Branch prefix detection
    let mut prefix_freq: HashMap<String, usize> = HashMap::new();
    for branch in &branches {
        if let Some((pfx, _)) = branch.split_once('/') {
            *prefix_freq.entry(format!("{pfx}/")).or_default() += 1;
        }
    }
    let mut branch_prefixes = freq_sorted(prefix_freq);
    branch_prefixes.truncate(10);

    // Risky path detection
    let risky_paths = detect_risky_paths(repo);

    let built_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Ok(RepoMemory {
        built_at,
        commit_style_hint,
        observed_types,
        observed_scopes,
        branch_prefixes,
        risky_paths,
    })
}

/// Load from cache or build fresh. Never propagates errors — returns None silently.
pub fn load_or_build(repo: &GitRepo) -> Option<RepoMemory> {
    let root = repo.repo_root().ok()?;
    let path = memory_path(&root).ok()?;

    if let Some(existing) = load_memory(&path) {
        if is_fresh(&existing) {
            return Some(existing);
        }
    }

    let memory = build_memory(repo).ok()?;
    let _ = save_memory(&path, &memory);
    Some(memory)
}

/// Force a rebuild regardless of staleness. Used by `gg learn`.
pub fn force_build(repo: &GitRepo) -> Result<RepoMemory> {
    let root = repo.repo_root()?;
    let path = memory_path(&root)?;
    let memory = build_memory(repo)?;
    save_memory(&path, &memory)?;
    Ok(memory)
}
