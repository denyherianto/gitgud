use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    ai::{
        AiClient, AiConfig, CommitMemoryAnalysis, CommitMemoryPromptInput,
        build_heuristic_commit_memory,
    },
    git::{CommitDetails, GitRepo},
    memory::{memory_dir, path_to_slug},
};

const DEFAULT_BACKFILL_LIMIT: usize = 50;
const DEFAULT_RESULT_LIMIT: usize = 10;
const STALE_COMMIT_WINDOW: usize = 15;
const HOOK_MARKER: &str = "# gitgud git memory hook";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitMemoryRecord {
    pub sha: String,
    pub subject: String,
    pub body: String,
    pub feature: String,
    pub what_changed: Vec<String>,
    pub why: Vec<String>,
    pub related_files: Vec<String>,
    pub committed_at: i64,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct CommitMemoryIndex {
    commits: BTreeMap<String, CommitMemoryIndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CommitMemoryIndexEntry {
    sha: String,
    subject: String,
    feature: String,
    what_changed: Vec<String>,
    why: Vec<String>,
    related_files: Vec<String>,
    committed_at: i64,
    recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillSummary {
    pub analyzed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookInstallStatus {
    Installed(PathBuf),
    Updated(PathBuf),
    AlreadyInstalled(PathBuf),
    Conflict(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub sha: String,
    pub subject: String,
    pub feature: String,
    pub what_changed: String,
    pub why: String,
    pub related_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactReport {
    pub file: String,
    pub features: Vec<String>,
    pub commits: Vec<SearchResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleCandidate {
    pub file: String,
    pub feature: String,
    pub last_touch_sha: String,
    pub last_touch_subject: String,
    pub commits_since_touch: usize,
    pub reason: String,
}

impl From<&CommitMemoryRecord> for CommitMemoryIndexEntry {
    fn from(record: &CommitMemoryRecord) -> Self {
        Self {
            sha: record.sha.clone(),
            subject: record.subject.clone(),
            feature: record.feature.clone(),
            what_changed: record.what_changed.clone(),
            why: record.why.clone(),
            related_files: record.related_files.clone(),
            committed_at: record.committed_at,
            recorded_at: record.recorded_at,
        }
    }
}

#[derive(Debug, Clone)]
enum CommitMemoryGenerator {
    Ai(AiClient),
    HeuristicOnly,
}

impl CommitMemoryGenerator {
    fn load() -> Self {
        match AiConfig::load().and_then(AiClient::new) {
            Ok(client) => Self::Ai(client),
            Err(_) => Self::HeuristicOnly,
        }
    }

    async fn analyze(&self, input: &CommitMemoryPromptInput) -> CommitMemoryAnalysis {
        match self {
            Self::Ai(client) => client
                .generate_commit_memory(input)
                .await
                .unwrap_or_else(|_| build_heuristic_commit_memory(input)),
            Self::HeuristicOnly => build_heuristic_commit_memory(input),
        }
    }
}

pub fn git_memory_dir(root: &Path) -> Result<PathBuf> {
    Ok(storage_base_dir()?
        .join(path_to_slug(root))
        .join("git-memory"))
}

pub fn index_path(root: &Path) -> Result<PathBuf> {
    Ok(git_memory_dir(root)?.join("index.json"))
}

pub fn records_dir(root: &Path) -> Result<PathBuf> {
    Ok(git_memory_dir(root)?.join("commits"))
}

pub fn record_path(root: &Path, sha: &str) -> Result<PathBuf> {
    Ok(records_dir(root)?.join(format!("{sha}.json")))
}

pub fn has_records(repo: &GitRepo) -> Result<bool> {
    let root = repo.repo_root()?;
    let index = load_index(&root)?;
    Ok(!index.commits.is_empty())
}

pub fn cached_recent_context(repo: &GitRepo, limit: usize) -> Result<Vec<String>> {
    let root = repo.repo_root()?;
    let index = load_index(&root)?;
    Ok(sorted_entries(&index)
        .into_iter()
        .take(limit)
        .map(format_context_line)
        .collect())
}

pub async fn ensure_recent_history(repo: &GitRepo, limit: usize) -> Result<()> {
    if !has_records(repo)? {
        let _ = backfill_recent(repo, limit, false).await?;
    }
    Ok(())
}

pub async fn backfill_recent(repo: &GitRepo, limit: usize, force: bool) -> Result<BackfillSummary> {
    let generator = CommitMemoryGenerator::load();
    let root = repo.repo_root()?;
    let mut analyzed = 0usize;
    let mut skipped = 0usize;

    for commit in repo.recent_commits(limit)? {
        if !force && load_record(&root, &commit.sha).is_some() {
            skipped += 1;
            continue;
        }

        let details = repo.commit_details(&commit.sha)?;
        let record = analyze_commit(&generator, &details).await;
        save_record_and_index(&root, &record)?;
        analyzed += 1;
    }

    Ok(BackfillSummary { analyzed, skipped })
}

pub async fn ingest_commit(repo: &GitRepo, reference: &str) -> Result<CommitMemoryRecord> {
    let generator = CommitMemoryGenerator::load();
    ingest_commit_with_generator(repo, &generator, reference, false).await
}

pub async fn explain_commit(repo: &GitRepo, reference: &str) -> Result<CommitMemoryRecord> {
    ensure_recent_history(repo, DEFAULT_BACKFILL_LIMIT).await?;
    let generator = CommitMemoryGenerator::load();
    ingest_commit_with_generator(repo, &generator, reference, false).await
}

pub async fn context_for_refs(
    repo: &GitRepo,
    refs: &[String],
    limit: usize,
) -> Result<Vec<String>> {
    if refs.is_empty() {
        return Ok(Vec::new());
    }

    let generator = CommitMemoryGenerator::load();
    let root = repo.repo_root()?;
    let mut lines = Vec::new();

    for reference in refs.iter().take(limit) {
        let record = ingest_commit_with_generator(repo, &generator, reference, false).await?;
        lines.push(format_context_record(&record));
    }

    if lines.is_empty() && load_index(&root)?.commits.is_empty() {
        return Ok(Vec::new());
    }

    Ok(lines)
}

pub async fn search(repo: &GitRepo, query: &str) -> Result<Vec<SearchResult>> {
    ensure_recent_history(repo, DEFAULT_BACKFILL_LIMIT).await?;
    let root = repo.repo_root()?;
    let index = load_index(&root)?;
    let normalized = query.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok(Vec::new());
    }

    let terms = normalized
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    let mut scored = sorted_entries(&index)
        .into_iter()
        .filter_map(|entry| {
            let score = score_entry(entry, &normalized, &terms);
            if score == 0 {
                return None;
            }
            Some((score, to_search_result(entry)))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.sha.cmp(&left.1.sha)));
    scored.truncate(DEFAULT_RESULT_LIMIT);
    Ok(scored.into_iter().map(|(_, result)| result).collect())
}

pub async fn impact(repo: &GitRepo, file: &str) -> Result<Option<ImpactReport>> {
    ensure_recent_history(repo, DEFAULT_BACKFILL_LIMIT).await?;
    let root = repo.repo_root()?;
    let index = load_index(&root)?;
    let normalized = normalize_repo_relative_path(&root, file);

    let commits = sorted_entries(&index)
        .into_iter()
        .filter(|entry| {
            entry
                .related_files
                .iter()
                .any(|related| related == &normalized)
        })
        .map(to_search_result)
        .collect::<Vec<_>>();

    if commits.is_empty() {
        return Ok(None);
    }

    let mut feature_counts = BTreeMap::<String, usize>::new();
    for commit in &commits {
        *feature_counts.entry(commit.feature.clone()).or_default() += 1;
    }
    let mut features = feature_counts.into_iter().collect::<Vec<_>>();
    features.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));

    Ok(Some(ImpactReport {
        file: normalized,
        features: features.into_iter().map(|(feature, _)| feature).collect(),
        commits,
    }))
}

pub async fn stale(repo: &GitRepo) -> Result<Vec<StaleCandidate>> {
    ensure_recent_history(repo, DEFAULT_BACKFILL_LIMIT).await?;
    let root = repo.repo_root()?;
    let index = load_index(&root)?;
    let sorted = sorted_entries(&index);
    if sorted.len() <= STALE_COMMIT_WINDOW {
        return Ok(Vec::new());
    }

    let mut latest_touch = BTreeMap::<String, (usize, &CommitMemoryIndexEntry)>::new();
    let mut touch_count = BTreeMap::<String, usize>::new();
    for (position, entry) in sorted.iter().enumerate() {
        for related in &entry.related_files {
            latest_touch
                .entry(related.clone())
                .or_insert((position, *entry));
            *touch_count.entry(related.clone()).or_default() += 1;
        }
    }

    let mut candidates = Vec::new();
    for (file, (position, entry)) in latest_touch {
        if position < STALE_COMMIT_WINDOW {
            continue;
        }
        if !root.join(&file).exists() {
            continue;
        }

        let change_count = touch_count.get(&file).copied().unwrap_or(0);
        let reason = if change_count <= 1 {
            format!("Only seen once and untouched in the last {position} analyzed commits.")
        } else {
            format!(
                "Untouched in the last {position} analyzed commits; last tied to feature '{}'.",
                entry.feature
            )
        };

        candidates.push(StaleCandidate {
            file,
            feature: entry.feature.clone(),
            last_touch_sha: entry.sha.clone(),
            last_touch_subject: entry.subject.clone(),
            commits_since_touch: position,
            reason,
        });
    }

    candidates.sort_by(|left, right| {
        right
            .commits_since_touch
            .cmp(&left.commits_since_touch)
            .then(left.file.cmp(&right.file))
    });
    candidates.truncate(DEFAULT_RESULT_LIMIT);
    Ok(candidates)
}

pub fn install_post_commit_hook(repo: &GitRepo) -> Result<HookInstallStatus> {
    let git_dir = repo.git_dir()?;
    let hook_path = git_dir.join("hooks").join("post-commit");
    let hook_script = build_hook_script()?;

    if let Some(parent) = hook_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create hooks directory {}", parent.display()))?;
    }

    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path)
            .with_context(|| format!("failed to read {}", hook_path.display()))?;
        if existing.contains(HOOK_MARKER) {
            if existing == hook_script {
                return Ok(HookInstallStatus::AlreadyInstalled(hook_path));
            }
            fs::write(&hook_path, &hook_script)
                .with_context(|| format!("failed to update {}", hook_path.display()))?;
            set_hook_permissions(&hook_path)?;
            return Ok(HookInstallStatus::Updated(hook_path));
        }
        return Ok(HookInstallStatus::Conflict(hook_path));
    }

    fs::write(&hook_path, hook_script)
        .with_context(|| format!("failed to write {}", hook_path.display()))?;
    set_hook_permissions(&hook_path)?;
    Ok(HookInstallStatus::Installed(hook_path))
}

async fn ingest_commit_with_generator(
    repo: &GitRepo,
    generator: &CommitMemoryGenerator,
    reference: &str,
    force: bool,
) -> Result<CommitMemoryRecord> {
    let root = repo.repo_root()?;
    let sha = repo.resolve_ref(reference)?;
    if !force {
        if let Some(existing) = load_record(&root, &sha) {
            return Ok(existing);
        }
    }

    let details = repo.commit_details(&sha)?;
    let record = analyze_commit(generator, &details).await;
    save_record_and_index(&root, &record)?;
    Ok(record)
}

async fn analyze_commit(
    generator: &CommitMemoryGenerator,
    details: &CommitDetails,
) -> CommitMemoryRecord {
    let input = CommitMemoryPromptInput {
        sha: details.sha.clone(),
        subject: details.subject.clone(),
        body: details.body.clone(),
        changed_files: details.changed_files.clone(),
        diff_stat: details.diff_stat.clone(),
        diff: details.diff.clone(),
    };
    let analysis = generator.analyze(&input).await;
    let related_files = normalize_related_files(&analysis.related_files, &details.changed_files);

    CommitMemoryRecord {
        sha: details.sha.clone(),
        subject: details.subject.clone(),
        body: details.body.clone(),
        feature: normalize_feature(&analysis.feature, &details.changed_files, &details.subject),
        what_changed: normalize_items(analysis.what_changed, &details.subject),
        why: normalize_items(
            analysis.why,
            &format!("Likely intended to {}", subject_to_intent(&details.subject)),
        ),
        related_files,
        committed_at: details.committed_at,
        recorded_at: now_unix_secs(),
    }
}

fn normalize_feature(raw: &str, changed_files: &[String], subject: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }

    changed_files
        .iter()
        .find_map(|path| path.split('/').next())
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| subject_to_intent(subject))
}

fn normalize_items(mut items: Vec<String>, fallback: &str) -> Vec<String> {
    items.retain(|item| !item.trim().is_empty());
    if items.is_empty() {
        vec![fallback.trim().to_string()]
    } else {
        items
            .into_iter()
            .map(|item| item.trim().trim_start_matches("- ").trim().to_string())
            .collect()
    }
}

fn normalize_related_files(suggested: &[String], changed_files: &[String]) -> Vec<String> {
    let mut files = suggested
        .iter()
        .map(|file| file.trim())
        .filter(|file| !file.is_empty())
        .filter(|file| changed_files.iter().any(|changed| changed == file))
        .map(str::to_string)
        .collect::<Vec<_>>();

    if files.is_empty() {
        return changed_files.to_vec();
    }

    files.sort();
    files.dedup();
    files
}

fn subject_to_intent(subject: &str) -> String {
    let normalized = subject
        .trim()
        .trim_end_matches('.')
        .trim_matches('"')
        .to_ascii_lowercase();
    if normalized.is_empty() {
        "improve the code".to_string()
    } else {
        normalized
    }
}

fn build_hook_script() -> Result<String> {
    let executable = env::current_exe().context("failed to resolve current executable path")?;
    let escaped = shell_escape_single(&executable.display().to_string());
    Ok(format!(
        "#!/bin/sh\n{HOOK_MARKER}\n{escaped} memory ingest --commit HEAD >/dev/null 2>&1 || true\n"
    ))
}

fn storage_base_dir() -> Result<PathBuf> {
    if let Ok(override_dir) = env::var("GITGUD_MEMORY_DIR") {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    memory_dir()
}

fn shell_escape_single(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn set_hook_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to set executable bit on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_hook_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn load_index(root: &Path) -> Result<CommitMemoryIndex> {
    let path = index_path(root)?;
    if !path.exists() {
        return Ok(CommitMemoryIndex::default());
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn save_index(root: &Path, index: &CommitMemoryIndex) -> Result<()> {
    let path = index_path(root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw =
        serde_json::to_string_pretty(index).context("failed to serialize git memory index")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn load_record(root: &Path, sha: &str) -> Option<CommitMemoryRecord> {
    let path = record_path(root, sha).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_record(root: &Path, record: &CommitMemoryRecord) -> Result<()> {
    let path = record_path(root, &record.sha)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let raw =
        serde_json::to_string_pretty(record).context("failed to serialize git memory record")?;
    fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn save_record_and_index(root: &Path, record: &CommitMemoryRecord) -> Result<()> {
    save_record(root, record)?;
    let mut index = load_index(root)?;
    index
        .commits
        .insert(record.sha.clone(), CommitMemoryIndexEntry::from(record));
    save_index(root, &index)?;
    Ok(())
}

fn sorted_entries(index: &CommitMemoryIndex) -> Vec<&CommitMemoryIndexEntry> {
    let mut entries = index.commits.values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .committed_at
            .cmp(&left.committed_at)
            .then(right.sha.cmp(&left.sha))
    });
    entries
}

fn format_context_line(entry: &CommitMemoryIndexEntry) -> String {
    format!(
        "{} | {} | {} | {}",
        shorten_sha(&entry.sha),
        entry.feature,
        first_line(&entry.what_changed),
        first_line(&entry.why)
    )
}

fn format_context_record(record: &CommitMemoryRecord) -> String {
    format!(
        "{} | {} | {} | {}",
        shorten_sha(&record.sha),
        record.feature,
        first_line(&record.what_changed),
        first_line(&record.why)
    )
}

fn to_search_result(entry: &CommitMemoryIndexEntry) -> SearchResult {
    SearchResult {
        sha: entry.sha.clone(),
        subject: entry.subject.clone(),
        feature: entry.feature.clone(),
        what_changed: first_line(&entry.what_changed),
        why: first_line(&entry.why),
        related_files: entry.related_files.clone(),
    }
}

fn first_line(lines: &[String]) -> String {
    lines
        .first()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "(none)".to_string())
}

fn score_entry(entry: &CommitMemoryIndexEntry, query: &str, terms: &[&str]) -> usize {
    let mut score = 0usize;
    let feature = entry.feature.to_ascii_lowercase();
    let subject = entry.subject.to_ascii_lowercase();
    let what = entry.what_changed.join(" ").to_ascii_lowercase();
    let why = entry.why.join(" ").to_ascii_lowercase();
    let files = entry.related_files.join(" ").to_ascii_lowercase();

    if feature.contains(query) {
        score += 8;
    }
    if subject.contains(query) {
        score += 5;
    }
    if what.contains(query) {
        score += 4;
    }
    if why.contains(query) {
        score += 4;
    }
    if files.contains(query) {
        score += 6;
    }

    for term in terms {
        if feature.contains(term) {
            score += 3;
        }
        if subject.contains(term) {
            score += 2;
        }
        if what.contains(term) {
            score += 2;
        }
        if why.contains(term) {
            score += 2;
        }
        if files.contains(term) {
            score += 2;
        }
    }

    score
}

fn normalize_repo_relative_path(root: &Path, raw: &str) -> String {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        if let Ok(stripped) = candidate.strip_prefix(root) {
            return path_to_forward_slashes(stripped);
        }
    }
    path_to_forward_slashes(candidate)
}

fn path_to_forward_slashes(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn shorten_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        path::{Path, PathBuf},
        process::Command,
        sync::OnceLock,
    };

    use tempfile::TempDir;

    use super::{
        CommitMemoryIndexEntry, CommitMemoryRecord, HookInstallStatus, build_hook_script,
        git_memory_dir, install_post_commit_hook, load_index, normalize_repo_relative_path,
        record_path, save_record_and_index, score_entry, stale,
    };
    use crate::git::GitRepo;

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("failed to run git {:?}: {error}", args));

        if !output.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        String::from_utf8(output.stdout).unwrap()
    }

    fn init_repo() -> TempDir {
        configure_test_memory_dir();
        let temp = TempDir::new().unwrap();
        run(temp.path(), &["init", "-b", "main"]);
        run(temp.path(), &["config", "user.name", "Test User"]);
        run(temp.path(), &["config", "user.email", "test@example.com"]);
        fs::write(temp.path().join("README.md"), "hello\n").unwrap();
        run(temp.path(), &["add", "README.md"]);
        run(temp.path(), &["commit", "-m", "Initial commit"]);
        temp
    }

    fn configure_test_memory_dir() {
        static MEMORY_BASE: OnceLock<PathBuf> = OnceLock::new();
        let base = MEMORY_BASE.get_or_init(|| {
            let path = std::env::temp_dir().join("gitgud-memory-tests");
            fs::create_dir_all(&path).unwrap();
            path
        });
        // SAFETY: tests share one deterministic temp root, so there is no race over value choice.
        unsafe {
            env::set_var("GITGUD_MEMORY_DIR", base);
        }
    }

    fn record(sha: &str, committed_at: i64, file: &str, feature: &str) -> CommitMemoryRecord {
        CommitMemoryRecord {
            sha: sha.to_string(),
            subject: format!("Update {feature}"),
            body: String::new(),
            feature: feature.to_string(),
            what_changed: vec![format!("Changed {file}")],
            why: vec![format!("Likely intended to improve {feature}")],
            related_files: vec![file.to_string()],
            committed_at,
            recorded_at: committed_at as u64,
        }
    }

    #[test]
    fn installs_git_memory_hook_in_empty_repo() {
        let repo_dir = init_repo();
        let repo = GitRepo::new(repo_dir.path());
        let status = install_post_commit_hook(&repo).unwrap();

        match status {
            HookInstallStatus::Installed(path) => {
                let raw = fs::read_to_string(path).unwrap();
                assert!(raw.contains("gitgud git memory hook"));
                assert!(raw.contains("memory ingest --commit HEAD"));
            }
            other => panic!("unexpected hook status: {other:?}"),
        }
    }

    #[test]
    fn refuses_to_overwrite_foreign_post_commit_hook() {
        let repo_dir = init_repo();
        let repo = GitRepo::new(repo_dir.path());
        let hook_path = repo.git_dir().unwrap().join("hooks").join("post-commit");
        fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
        fs::write(&hook_path, "#!/bin/sh\necho custom\n").unwrap();

        let status = install_post_commit_hook(&repo).unwrap();
        assert_eq!(status, HookInstallStatus::Conflict(hook_path));
    }

    #[test]
    fn saves_hybrid_index_and_record_files() {
        let repo_dir = init_repo();
        let repo = GitRepo::new(repo_dir.path());
        let root = repo.repo_root().unwrap();
        let record = record("abc1234", 10, "src/lib.rs", "billing");
        save_record_and_index(&root, &record).unwrap();

        assert!(record_path(&root, "abc1234").unwrap().exists());
        let index = load_index(&root).unwrap();
        assert!(index.commits.contains_key("abc1234"));
    }

    #[test]
    fn normalizes_absolute_paths_into_repo_relative_paths() {
        let repo_dir = init_repo();
        let absolute = repo_dir.path().join("src/lib.rs");
        let normalized = normalize_repo_relative_path(repo_dir.path(), absolute.to_str().unwrap());
        assert_eq!(normalized, "src/lib.rs");
    }

    #[test]
    fn scores_feature_matches_higher_than_loose_text() {
        let entry = CommitMemoryIndexEntry {
            sha: "abc1234".into(),
            subject: "Refine billing sidebar".into(),
            feature: "billing".into(),
            what_changed: vec!["Updates billing sidebar".into()],
            why: vec!["Likely intended to improve billing navigation".into()],
            related_files: vec!["src/billing.rs".into()],
            committed_at: 10,
            recorded_at: 10,
        };

        let feature_score = score_entry(&entry, "billing", &["billing"]);
        let unrelated_score = score_entry(&entry, "search", &["search"]);
        assert!(feature_score > unrelated_score);
    }

    #[tokio::test]
    async fn stale_reports_files_not_touched_in_recent_commit_window() {
        let repo_dir = init_repo();
        let repo = GitRepo::new(repo_dir.path());
        let root = repo.repo_root().unwrap();
        fs::create_dir_all(repo_dir.path().join("src")).unwrap();
        fs::write(repo_dir.path().join("src/legacy.rs"), "fn legacy() {}\n").unwrap();

        for index in 0..18 {
            let file = if index == 17 {
                "src/legacy.rs"
            } else {
                "src/active.rs"
            };
            let feature = if index == 17 { "legacy" } else { "active" };
            save_record_and_index(
                &root,
                &record(
                    &format!("{index:040x}"),
                    (100 - index) as i64,
                    file,
                    feature,
                ),
            )
            .unwrap();
        }

        let report = stale(&repo).await.unwrap();
        assert!(
            report
                .iter()
                .any(|candidate| candidate.file == "src/legacy.rs")
        );
    }

    #[test]
    fn builds_paths_under_repo_specific_git_memory_directory() {
        let repo_dir = init_repo();
        let path = git_memory_dir(repo_dir.path()).unwrap();
        assert!(path.ends_with("git-memory"));
    }

    #[test]
    fn builds_hook_script_with_ingest_command() {
        let script = build_hook_script().unwrap();
        assert!(script.contains("memory ingest --commit HEAD"));
        assert!(script.contains("#!/bin/sh"));
    }
}
