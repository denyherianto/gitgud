use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Output, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::ai::SplitCommitPlan;

#[derive(Debug, Clone)]
pub struct GitRepo {
    cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    pub branch: Option<String>,
    pub staged_files: Vec<String>,
    pub staged_count: usize,
    pub unstaged_count: usize,
    pub remotes: Vec<String>,
    pub tracking: Option<String>,
    pub has_upstream: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedChanges {
    pub staged_files: Vec<String>,
    pub diff_stat: String,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushPlan {
    Upstream { branch: String },
    SetUpstream { remote: String, branch: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafeDiffWarning {
    pub message: String,
}

impl GitRepo {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn ensure_git_available(&self) -> Result<()> {
        self.run_checked(["--version"]).map(|_| ())
    }

    pub fn ensure_repo(&self) -> Result<()> {
        let inside = self.run_checked(["rev-parse", "--is-inside-work-tree"])?;
        if inside.trim() == "true" {
            Ok(())
        } else {
            bail!("current directory is not inside a git work tree");
        }
    }

    pub fn branch_name(&self) -> Result<Option<String>> {
        let branch = self.run_checked(["branch", "--show-current"])?;
        let branch = branch.trim();

        if branch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(branch.to_string()))
        }
    }

    pub fn current_branch(&self) -> Result<String> {
        self.branch_name()?
            .ok_or_else(|| anyhow!("detached HEAD is not supported for this command"))
    }

    pub fn list_remotes(&self) -> Result<Vec<String>> {
        let output = self.run_checked(["remote"])?;
        Ok(output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    pub fn has_upstream(&self) -> bool {
        self.run_raw(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    pub fn status(&self) -> Result<RepoStatus> {
        let porcelain = self.run_checked(["status", "--porcelain=1", "--branch"])?;
        let remotes = self.list_remotes()?;
        let branch = self.branch_name()?;
        let has_upstream = branch.is_some() && self.has_upstream();
        let tracking = parse_tracking(&porcelain);

        let mut staged_files = Vec::new();
        let mut unstaged_count = 0;

        for line in porcelain.lines().skip(1).filter(|line| !line.is_empty()) {
            if let Some(entry) = parse_status_entry(line) {
                if entry.staged {
                    staged_files.push(entry.path.clone());
                }
                if entry.unstaged {
                    unstaged_count += 1;
                }
            }
        }

        Ok(RepoStatus {
            branch,
            staged_count: staged_files.len(),
            staged_files,
            unstaged_count,
            remotes,
            tracking,
            has_upstream,
        })
    }

    pub fn staged_changes(&self) -> Result<StagedChanges> {
        let staged_files = self
            .run_checked(["diff", "--cached", "--name-only"])?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();

        let diff_stat = self.run_checked(["diff", "--cached", "--stat", "--compact-summary"])?;
        let diff = self.run_checked(["diff", "--cached", "--patch", "--no-ext-diff"])?;

        Ok(StagedChanges {
            staged_files,
            diff_stat,
            diff,
        })
    }

    pub fn plan_push(&self) -> Result<PushPlan> {
        let branch = self.current_branch()?;
        let remotes = self.list_remotes()?;
        resolve_push_plan(&branch, self.has_upstream(), remotes)
    }

    pub fn staged_diff_warnings(&self) -> Result<Vec<UnsafeDiffWarning>> {
        let snapshot = self.diff_snapshot(&["diff", "--cached"])?;
        Ok(collect_unsafe_diff_warnings(&snapshot))
    }

    pub fn push_diff_warnings(&self, plan: &PushPlan) -> Result<Vec<UnsafeDiffWarning>> {
        let base = self.push_diff_base(plan)?;
        let snapshot = self.diff_snapshot(&["diff", base.as_str(), "HEAD"])?;
        Ok(collect_unsafe_diff_warnings(&snapshot))
    }

    pub fn commit(&self, message: &str) -> Result<String> {
        let mut command = Command::new("git");
        command
            .current_dir(&self.cwd)
            .args(["commit", "-F", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().context("failed to start git commit")?;
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            stdin
                .write_all(message.as_bytes())
                .context("failed to write commit message to git commit")?;
        }

        let output = child
            .wait_with_output()
            .context("failed to wait for git commit")?;
        stringify_output(output, "git commit")
    }

    pub fn stage_all(&self) -> Result<String> {
        self.run_checked(["add", "--all"])
    }

    pub fn stage_paths(&self, paths: &[String]) -> Result<String> {
        if paths.is_empty() {
            bail!("cannot stage an empty file selection");
        }

        let mut args = Vec::with_capacity(paths.len() + 2);
        args.push("add");
        args.push("--");
        for path in paths {
            args.push(path.as_str());
        }
        self.run_checked_slice(&args)
    }

    pub fn clear_staging(&self) -> Result<String> {
        self.run_checked(["reset", "--mixed", "--quiet"])
    }

    pub fn split_commit(&self, plans: &[SplitCommitPlan]) -> Result<()> {
        if plans.len() < 2 {
            bail!("split commit requires at least two commits");
        }

        let staged = self.staged_changes()?;
        if staged.staged_files.is_empty() {
            bail!("no staged changes found");
        }

        validate_split_plan(plans, &staged.staged_files)?;

        self.clear_staging()?;

        let mut committed = 0usize;
        for plan in plans {
            if let Err(error) = self
                .stage_paths(&plan.files)
                .and_then(|_| self.commit(&plan.message).map(|_| ()))
            {
                if committed == 0 {
                    let _ = self.stage_paths(&staged.staged_files);
                    return Err(error.context("split commit failed before creating any commits"));
                }

                bail!("split commit stopped after {committed} commits: {error}");
            }

            committed += 1;
        }

        Ok(())
    }

    pub fn run_passthrough(&self, args: &[OsString]) -> Result<ExitStatus> {
        Command::new("git")
            .current_dir(&self.cwd)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("failed to execute {}", format_git_command(args)))
    }

    pub fn push(&self, plan: &PushPlan) -> Result<String> {
        self.push_with_options(plan, false)
    }

    pub fn push_with_force_lease(&self, plan: &PushPlan) -> Result<String> {
        self.push_with_options(plan, true)
    }

    fn push_with_options(&self, plan: &PushPlan, force_with_lease: bool) -> Result<String> {
        match plan {
            PushPlan::Upstream { .. } => {
                if force_with_lease {
                    self.run_checked_slice(&["push", "--force-with-lease"])
                } else {
                    self.run_checked(["push"])
                }
            }
            PushPlan::SetUpstream { remote, branch } => {
                if force_with_lease {
                    self.run_checked_slice(&[
                        "push",
                        "--force-with-lease",
                        "-u",
                        remote.as_str(),
                        branch.as_str(),
                    ])
                } else {
                    self.run_checked_slice(&["push", "-u", remote.as_str(), branch.as_str()])
                }
            }
        }
    }

    pub fn push_to_remote(&self, remote: &str, branch: &str) -> Result<String> {
        self.run_checked(["push", "-u", remote, branch])
    }

    fn diff_snapshot(&self, diff_args: &[&str]) -> Result<DiffSnapshot> {
        let mut name_only_args = diff_args.to_vec();
        name_only_args.push("--name-only");
        let changed_files = self
            .run_checked_slice(&name_only_args)?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();

        let mut patch_args = diff_args.to_vec();
        patch_args.extend(["--patch", "--no-ext-diff"]);
        let diff = self.run_checked_slice(&patch_args)?;

        Ok(DiffSnapshot {
            changed_files,
            diff,
        })
    }

    fn push_diff_base(&self, plan: &PushPlan) -> Result<String> {
        match plan {
            PushPlan::Upstream { .. } => Ok("@{u}".to_string()),
            PushPlan::SetUpstream { remote, branch } => {
                let remote_ref = format!("refs/remotes/{remote}/{branch}");
                let args = ["rev-parse", "--verify", remote_ref.as_str()];
                match self.run_checked_slice(&args) {
                    Ok(output) => Ok(output.trim().to_string()),
                    Err(_) => Ok(EMPTY_TREE_HASH.to_string()),
                }
            }
        }
    }

    fn run_checked<const N: usize>(&self, args: [&str; N]) -> Result<String> {
        let output = self.run_raw(args)?;
        stringify_output(output, &format!("git {}", args.join(" ")))
    }

    fn run_checked_slice(&self, args: &[&str]) -> Result<String> {
        let output = self.run_raw_slice(args)?;
        stringify_output(output, &format!("git {}", args.join(" ")))
    }

    fn run_raw<const N: usize>(&self, args: [&str; N]) -> Result<Output> {
        Command::new("git")
            .current_dir(&self.cwd)
            .args(args)
            .output()
            .with_context(|| format!("failed to execute git {}", args.join(" ")))
    }

    fn run_raw_slice(&self, args: &[&str]) -> Result<Output> {
        Command::new("git")
            .current_dir(&self.cwd)
            .args(args)
            .output()
            .with_context(|| format!("failed to execute git {}", args.join(" ")))
    }
}

fn format_git_command(args: &[OsString]) -> String {
    let rendered = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");

    if rendered.is_empty() {
        "git".to_string()
    } else {
        format!("git {rendered}")
    }
}

fn validate_split_plan(plans: &[SplitCommitPlan], staged_files: &[String]) -> Result<()> {
    let expected = staged_files.iter().cloned().collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();

    for plan in plans {
        if plan.message.trim().is_empty() {
            bail!("split commit messages cannot be empty");
        }
        if plan.files.is_empty() {
            bail!("split commits must include at least one file");
        }

        for file in &plan.files {
            if !expected.contains(file) {
                bail!("split commit referenced an unstaged file: {file}");
            }
            if !seen.insert(file.clone()) {
                bail!("split commit referenced a file more than once: {file}");
            }
        }
    }

    if seen != expected {
        bail!("split commits must cover every staged file exactly once");
    }

    Ok(())
}

// Canonical SHA-1 hash of Git's empty tree object, used as the diff base
// when a remote branch does not exist yet and the first push should be
// treated as introducing the full repository contents.
const EMPTY_TREE_HASH: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const HUGE_GENERATED_LINE_THRESHOLD: usize = 2_000;
const CONSOLE_LOG_WARNING_THRESHOLD: usize = 3;
const MINIFIED_LINE_LENGTH_THRESHOLD: usize = 500;
const MAX_WARNING_PATHS: usize = 3;

#[derive(Debug)]
struct DiffSnapshot {
    changed_files: Vec<String>,
    diff: String,
}

#[derive(Debug, Default)]
struct DiffFileStats {
    added_lines: usize,
    deleted_lines: usize,
    console_logs: usize,
    has_private_key: bool,
    has_env_assignment: bool,
    has_generated_marker: bool,
    has_minified_blob: bool,
}

fn collect_unsafe_diff_warnings(snapshot: &DiffSnapshot) -> Vec<UnsafeDiffWarning> {
    if snapshot.changed_files.is_empty() {
        return Vec::new();
    }

    let diff_by_path = split_diff_by_path(&snapshot.diff);
    let mut env_secret_files = Vec::new();
    let mut private_key_files = Vec::new();
    let mut huge_generated_files = Vec::new();
    let mut minified_files = Vec::new();
    let mut console_log_files = Vec::new();
    let mut console_log_count = 0usize;

    for path in &snapshot.changed_files {
        let diff = diff_by_path.get(path).map(String::as_str).unwrap_or("");
        let stats = analyze_file_diff(path, diff);

        if is_probably_secret_env_file(path) && stats.has_env_assignment {
            env_secret_files.push(path.clone());
        }
        if stats.has_private_key {
            private_key_files.push(path.clone());
        }
        if (is_generated_path(path) || stats.has_generated_marker)
            && stats.added_lines + stats.deleted_lines >= HUGE_GENERATED_LINE_THRESHOLD
        {
            huge_generated_files.push(path.clone());
        }
        if stats.has_minified_blob {
            minified_files.push(path.clone());
        }
        if stats.console_logs > 0 {
            console_log_count += stats.console_logs;
            console_log_files.push(path.clone());
        }
    }

    let mut warnings = Vec::with_capacity(6);
    if !env_secret_files.is_empty() {
        warnings.push(UnsafeDiffWarning {
            message: format!(
                "Potential secrets added in .env files: {}",
                summarize_paths(&env_secret_files)
            ),
        });
    }
    if !private_key_files.is_empty() {
        warnings.push(UnsafeDiffWarning {
            message: format!(
                "Private key material detected: {}",
                summarize_paths(&private_key_files)
            ),
        });
    }
    if !huge_generated_files.is_empty() {
        warnings.push(UnsafeDiffWarning {
            message: format!(
                "Large generated files detected: {}",
                summarize_paths(&huge_generated_files)
            ),
        });
    }
    if !minified_files.is_empty() {
        warnings.push(UnsafeDiffWarning {
            message: format!(
                "Minified blobs detected: {}",
                summarize_paths(&minified_files)
            ),
        });
    }
    if is_lockfile_only_change(&snapshot.changed_files) {
        warnings.push(UnsafeDiffWarning {
            message: format!(
                "Only lockfiles changed: {}",
                summarize_paths(&snapshot.changed_files)
            ),
        });
    }
    if console_log_count >= CONSOLE_LOG_WARNING_THRESHOLD {
        warnings.push(UnsafeDiffWarning {
            message: format!(
                "Added {console_log_count} console.log statements: {}",
                summarize_paths(&console_log_files)
            ),
        });
    }

    warnings
}

fn analyze_file_diff(path: &str, diff: &str) -> DiffFileStats {
    let mut stats = DiffFileStats::default();

    for line in diff.lines() {
        if !line.starts_with('+') || line.starts_with("+++") {
            if line.starts_with('-') && !line.starts_with("---") {
                stats.deleted_lines += 1;
            }
            continue;
        }

        let added = &line[1..];
        let trimmed = added.trim();
        let lowered = trimmed.to_ascii_lowercase();
        stats.added_lines += 1;

        if trimmed.starts_with("console.log(") || trimmed.contains(" console.log(") {
            stats.console_logs += 1;
        }
        if trimmed.contains("BEGIN ") && trimmed.contains("PRIVATE KEY") {
            stats.has_private_key = true;
        }
        if is_probably_secret_env_file(path) && is_env_assignment_line(trimmed) {
            stats.has_env_assignment = true;
        }
        if contains_generated_marker(&lowered) {
            stats.has_generated_marker = true;
        }
        if looks_minified_line(added) || path.ends_with(".min.js") || path.ends_with(".min.css") {
            stats.has_minified_blob = true;
        }
    }

    stats
}

fn split_diff_by_path(diff: &str) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut current_path: Option<String> = None;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            if let Some((_, path)) = rest.split_once(" b/") {
                let path = path.to_string();
                files.entry(path.clone()).or_insert_with(String::new);
                current_path = Some(path);
                continue;
            }
        }

        if let Some(path) = &current_path {
            let entry = files.entry(path.clone()).or_insert_with(String::new);
            entry.push_str(line);
            entry.push('\n');
        }
    }

    files
}

fn is_probably_secret_env_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    if !file_name.starts_with(".env") {
        return false;
    }

    let lowered = file_name.to_ascii_lowercase();
    !lowered.contains("example")
        && !lowered.contains("sample")
        && !lowered.contains("template")
        && !lowered.contains("defaults")
}

fn is_env_assignment_line(line: &str) -> bool {
    if line.is_empty() || line.starts_with('#') {
        return false;
    }

    let Some((key, value)) = line.split_once('=') else {
        return false;
    };

    !key.trim().is_empty() && !value.trim().is_empty()
}

fn contains_generated_marker(line: &str) -> bool {
    GENERATED_MARKERS.iter().any(|marker| line.contains(marker))
}

fn is_generated_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    GENERATED_PATH_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

fn looks_minified_line(line: &str) -> bool {
    if line.len() < MINIFIED_LINE_LENGTH_THRESHOLD {
        return false;
    }

    let whitespace = line.chars().filter(|ch| ch.is_whitespace()).count();
    whitespace * 20 < line.len()
}

fn is_lockfile_only_change(files: &[String]) -> bool {
    !files.is_empty() && files.iter().all(|path| is_lockfile_path(path))
}

fn is_lockfile_path(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or(path),
        "Cargo.lock"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lockb"
            | "Gemfile.lock"
            | "composer.lock"
            | "Podfile.lock"
            | "go.sum"
            | "Pipfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "flake.lock"
    )
}

fn summarize_paths(paths: &[String]) -> String {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.iter().any(|existing| existing == path) {
            unique.push(path.clone());
        }
    }

    let remaining = unique.len().saturating_sub(MAX_WARNING_PATHS);
    let mut rendered = unique
        .iter()
        .take(MAX_WARNING_PATHS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if remaining > 0 {
        rendered.push_str(&format!(" (+{remaining} more)"));
    }
    rendered
}

const GENERATED_PATH_MARKERS: &[&str] = &[
    "/dist/",
    "dist/",
    "/build/",
    "build/",
    "/coverage/",
    "coverage/",
    "/vendor/",
    "vendor/",
    "/node_modules/",
    "node_modules/",
    "/target/",
    "target/",
    "/out/",
    "out/",
    ".generated.",
    ".snap",
    ".map",
];

const GENERATED_MARKERS: &[&str] = &[
    "@generated",
    "@gen",
    "auto-generated",
    "automatically generated",
    "do not edit",
    "generated by",
];

pub fn resolve_push_plan(
    branch: &str,
    has_upstream: bool,
    remotes: Vec<String>,
) -> Result<PushPlan> {
    if has_upstream {
        return Ok(PushPlan::Upstream {
            branch: branch.to_string(),
        });
    }

    if remotes.iter().any(|remote| remote == "origin") {
        return Ok(PushPlan::SetUpstream {
            remote: "origin".to_string(),
            branch: branch.to_string(),
        });
    }

    if remotes.len() == 1 {
        return Ok(PushPlan::SetUpstream {
            remote: remotes[0].clone(),
            branch: branch.to_string(),
        });
    }

    if remotes.is_empty() {
        bail!("no remotes found; add a remote or configure an upstream before pushing");
    }

    bail!("push target is ambiguous; configure an upstream or keep a single remote named 'origin'")
}

pub fn push_needs_force_with_lease(error_message: &str) -> bool {
    let normalized = error_message.to_ascii_lowercase();
    normalized.contains("non-fast-forward")
        || normalized.contains("[rejected]")
        || normalized.contains("fetch first")
        || normalized.contains("stale info")
}

fn stringify_output(output: Output, context: &str) -> Result<String> {
    if output.status.success() {
        let stdout = String::from_utf8(output.stdout).context("git output was not valid UTF-8")?;
        if !stdout.trim().is_empty() {
            Ok(stdout)
        } else {
            Ok(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "{context} failed: {}",
            stderr.trim().if_empty(stdout.trim())
        )
    }
}

fn parse_tracking(status_output: &str) -> Option<String> {
    let line = status_output.lines().next()?;
    let tracking = line.split_once("...")?.1.trim();
    if tracking.is_empty() {
        None
    } else {
        Some(tracking.to_string())
    }
}

#[derive(Debug)]
struct StatusEntry {
    path: String,
    staged: bool,
    unstaged: bool,
}

fn parse_status_entry(line: &str) -> Option<StatusEntry> {
    if line.len() < 3 {
        return None;
    }

    let index = line.chars().next()?;
    let worktree = line.chars().nth(1)?;
    let raw_path = &line[3..];
    let path = raw_path
        .rsplit(" -> ")
        .next()
        .unwrap_or(raw_path)
        .trim()
        .to_string();

    Some(StatusEntry {
        path,
        staged: index != ' ' && index != '?',
        unstaged: worktree != ' ' || (index == '?' && worktree == '?'),
    })
}

trait EmptyFallback {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl EmptyFallback for str {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.is_empty() { fallback } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DiffSnapshot, PushPlan, UnsafeDiffWarning, collect_unsafe_diff_warnings,
        is_lockfile_only_change, looks_minified_line, parse_status_entry, parse_tracking,
        push_needs_force_with_lease, resolve_push_plan,
    };

    #[test]
    fn prefers_existing_upstream() {
        let actual = resolve_push_plan("main", true, vec!["origin".into()]).unwrap();
        assert_eq!(
            actual,
            PushPlan::Upstream {
                branch: "main".into()
            }
        );
    }

    #[test]
    fn prefers_origin_for_first_push() {
        let actual =
            resolve_push_plan("main", false, vec!["upstream".into(), "origin".into()]).unwrap();
        assert_eq!(
            actual,
            PushPlan::SetUpstream {
                remote: "origin".into(),
                branch: "main".into()
            }
        );
    }

    #[test]
    fn uses_single_remote_without_origin() {
        let actual = resolve_push_plan("main", false, vec!["mirror".into()]).unwrap();
        assert_eq!(
            actual,
            PushPlan::SetUpstream {
                remote: "mirror".into(),
                branch: "main".into()
            }
        );
    }

    #[test]
    fn rejects_ambiguous_first_push() {
        let error =
            resolve_push_plan("main", false, vec!["mirror".into(), "backup".into()]).unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn rejects_push_without_remotes() {
        let error = resolve_push_plan("main", false, Vec::new()).unwrap_err();
        assert!(error.to_string().contains("no remotes found"));
    }

    #[test]
    fn parses_untracked_status_as_unstaged() {
        let entry = parse_status_entry("?? src/main.rs").unwrap();
        assert!(!entry.staged);
        assert!(entry.unstaged);
        assert_eq!(entry.path, "src/main.rs");
    }

    #[test]
    fn parses_renamed_status_using_new_path() {
        let entry = parse_status_entry("R  src/old.rs -> src/new.rs").unwrap();
        assert!(entry.staged);
        assert!(!entry.unstaged);
        assert_eq!(entry.path, "src/new.rs");
    }

    #[test]
    fn parses_tracking_branch_from_status_header() {
        let tracking = parse_tracking("## main...origin/main [ahead 1]\nM  src/main.rs");
        assert_eq!(tracking.as_deref(), Some("origin/main [ahead 1]"));
    }

    #[test]
    fn returns_none_when_tracking_branch_is_missing() {
        assert_eq!(parse_tracking("## main\n"), None);
    }

    #[test]
    fn detects_force_with_lease_rejection_text() {
        assert!(push_needs_force_with_lease(
            "git push failed: ! [rejected] main -> main (non-fast-forward)"
        ));
    }

    #[test]
    fn detects_fetch_first_force_with_lease_hint() {
        assert!(push_needs_force_with_lease(
            "Updates were rejected because the remote contains work that you do not have locally. Fetch first."
        ));
    }

    fn warning_messages(snapshot: DiffSnapshot) -> Vec<String> {
        collect_unsafe_diff_warnings(&snapshot)
            .into_iter()
            .map(|warning: UnsafeDiffWarning| warning.message)
            .collect()
    }

    #[test]
    fn flags_secret_env_private_keys_and_console_logs() {
        let messages = warning_messages(DiffSnapshot {
            changed_files: vec![
                ".env".into(),
                "secrets/deploy.pem".into(),
                "src/app.js".into(),
            ],
            diff: r#"diff --git a/.env b/.env
+API_TOKEN=super-secret
diff --git a/secrets/deploy.pem b/secrets/deploy.pem
+-----BEGIN OPENSSH PRIVATE KEY-----
diff --git a/src/app.js b/src/app.js
+console.log("one");
+console.log("two");
+console.log("three");
"#
            .into(),
        });

        assert!(
            messages
                .iter()
                .any(|message| message.contains("Potential secrets added in .env files"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("Private key material detected"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("Added 3 console.log statements"))
        );
    }

    #[test]
    fn flags_large_generated_files_and_minified_blobs() {
        let large_block = "+// @generated\n".repeat(2_100);
        let minified_line = format!("+{}", "a".repeat(700));
        let diff = format!(
            "diff --git a/dist/schema.js b/dist/schema.js\n{large_block}diff --git a/public/app.min.js b/public/app.min.js\n{minified_line}\n"
        );
        let messages = warning_messages(DiffSnapshot {
            changed_files: vec!["dist/schema.js".into(), "public/app.min.js".into()],
            diff,
        });

        assert!(
            messages
                .iter()
                .any(|message| message.contains("Large generated files detected"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("Minified blobs detected"))
        );
    }

    #[test]
    fn flags_lockfile_only_changes() {
        let messages = warning_messages(DiffSnapshot {
            changed_files: vec!["Cargo.lock".into(), "pnpm-lock.yaml".into()],
            diff: String::new(),
        });

        assert!(
            messages
                .iter()
                .any(|message| message.contains("Only lockfiles changed"))
        );
    }

    #[test]
    fn ignores_example_env_files_for_secret_warning() {
        let messages = warning_messages(DiffSnapshot {
            changed_files: vec![".env.example".into()],
            diff: "diff --git a/.env.example b/.env.example\n+API_TOKEN=placeholder\n".into(),
        });

        assert!(
            !messages
                .iter()
                .any(|message| message.contains("Potential secrets added in .env files"))
        );
    }

    #[test]
    fn detects_lockfile_only_changes() {
        assert!(is_lockfile_only_change(&[
            "Cargo.lock".into(),
            "yarn.lock".into()
        ]));
        assert!(!is_lockfile_only_change(&[
            "Cargo.lock".into(),
            "README.md".into()
        ]));
    }

    #[test]
    fn detects_minified_lines_by_length_and_whitespace() {
        assert!(looks_minified_line(&"a".repeat(700)));
        assert!(!looks_minified_line("let value = 1;"));
    }
}
