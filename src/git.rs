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
pub struct BranchCommit {
    pub sha: String,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetails {
    pub sha: String,
    pub subject: String,
    pub body: String,
    pub committed_at: i64,
    pub changed_files: Vec<String>,
    pub diff_stat: String,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipRange {
    pub base_label: String,
    pub commits: Vec<BranchCommit>,
    pub changed_files: Vec<String>,
    pub diff_stat: String,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    pub selector: String,
    pub commit: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    pub reference: String,
    pub commit: String,
    pub summary: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShipBase {
    diff_base: String,
    commit_range: Option<String>,
    base_label: String,
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

    pub fn push_diff_warnings(
        &self,
        plan: &PushPlan,
        memory: Option<&crate::memory::RepoMemory>,
    ) -> Result<Vec<UnsafeDiffWarning>> {
        let base = self.push_diff_base(plan)?;
        let snapshot = self.diff_snapshot(&["diff", base.as_str(), "HEAD"])?;
        let mut warnings = collect_unsafe_diff_warnings(&snapshot);
        if let Some(mem) = memory {
            append_risky_path_warnings(&snapshot.changed_files, &mem.risky_paths, &mut warnings);
        }
        Ok(warnings)
    }

    pub fn ship_range(&self, plan: &PushPlan) -> Result<ShipRange> {
        let base = self.ship_base(plan)?;
        let diff_args = ["diff", base.diff_base.as_str(), "HEAD"];
        let snapshot = self.diff_snapshot(&diff_args)?;
        let diff_stat = self.run_checked([
            "diff",
            "--stat",
            "--compact-summary",
            base.diff_base.as_str(),
            "HEAD",
        ])?;
        let commits = self.ship_commits(&base)?;

        Ok(ShipRange {
            base_label: base.base_label,
            commits,
            changed_files: snapshot.changed_files,
            diff_stat,
            diff: snapshot.diff,
        })
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

    pub fn recent_log(&self, count: usize) -> Result<String> {
        let limit = format!("-{count}");
        let args = ["log", "--oneline", limit.as_str()];
        self.run_checked_slice(&args)
    }

    pub fn repo_root(&self) -> Result<PathBuf> {
        self.run_checked(["rev-parse", "--show-toplevel"])
            .map(|output| PathBuf::from(output.trim()))
    }

    pub fn recent_commits(&self, count: usize) -> Result<Vec<BranchCommit>> {
        let limit = format!("-{count}");
        let output =
            self.run_checked_slice(&["log", "--format=%H%x1f%s%x1f%b%x1e", limit.as_str()])?;
        Ok(parse_branch_commits(&output))
    }

    pub fn name_only_log(&self, count: usize) -> Result<String> {
        let limit = format!("-{count}");
        self.run_checked_slice(&["log", "--name-only", "--format=%x1e", limit.as_str()])
    }

    pub fn head_sha(&self) -> Result<String> {
        self.run_checked(["rev-parse", "HEAD"])
            .map(|output| output.trim().to_string())
    }

    pub fn commit_details(&self, reference: &str) -> Result<CommitDetails> {
        let sha = self.resolve_ref(reference)?;
        let header = self.run_checked_slice(&[
            "show",
            "-s",
            "--format=%H%x1f%s%x1f%b%x1f%ct",
            sha.as_str(),
        ])?;
        let mut fields = header.trim_end().split('\u{1f}');
        let resolved_sha = fields
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("git show did not return a commit sha"))?
            .to_string();
        let subject = fields.next().unwrap_or_default().trim().to_string();
        let body = fields.next().unwrap_or_default().trim().to_string();
        let committed_at = fields
            .next()
            .unwrap_or("0")
            .trim()
            .parse::<i64>()
            .unwrap_or(0);

        let changed_files = self
            .run_checked_slice(&["show", "--name-only", "--format=", sha.as_str()])?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let diff_stat = self.run_checked_slice(&[
            "show",
            "--stat",
            "--compact-summary",
            "--format=",
            sha.as_str(),
        ])?;
        let diff = self.run_checked_slice(&[
            "show",
            "--patch",
            "--no-ext-diff",
            "--format=",
            sha.as_str(),
        ])?;

        Ok(CommitDetails {
            sha: resolved_sha,
            subject,
            body,
            committed_at,
            changed_files,
            diff_stat,
            diff,
        })
    }

    pub fn git_dir(&self) -> Result<PathBuf> {
        self.run_checked(["rev-parse", "--git-dir"]).map(|output| {
            let path = output.trim();
            if Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                self.cwd.join(path)
            }
        })
    }

    pub fn rebase_in_progress(&self) -> Result<bool> {
        let git_dir = self.git_dir()?;
        Ok(git_dir.join("rebase-apply").exists() || git_dir.join("rebase-merge").exists())
    }

    pub fn local_branches(&self) -> Result<Vec<String>> {
        let output =
            self.run_checked(["for-each-ref", "--format=%(refname:short)", "refs/heads"])?;
        Ok(output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    pub fn remote_branches(&self) -> Result<Vec<String>> {
        let output =
            self.run_checked(["for-each-ref", "--format=%(refname:short)", "refs/remotes"])?;
        Ok(output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| !line.ends_with("/HEAD"))
            .map(str::to_string)
            .collect())
    }

    pub fn reflog(&self, reference: &str, count: usize) -> Result<Vec<ReflogEntry>> {
        let limit = format!("-{count}");
        let args = [
            "log",
            "-g",
            "--format=%gD%x1f%H%x1f%gs%x1e",
            limit.as_str(),
            reference,
        ];
        let output = self.run_checked_slice(&args)?;
        Ok(parse_reflog_entries(&output))
    }

    pub fn stash_entries(&self) -> Result<Vec<StashEntry>> {
        let output = self.run_checked(["stash", "list", "--format=%gd%x1f%H%x1f%gs%x1e"])?;
        Ok(parse_stash_entries(&output))
    }

    pub fn dropped_stash_candidates(&self) -> Result<Vec<StashEntry>> {
        let output =
            self.run_checked(["fsck", "--no-reflogs", "--unreachable", "--no-progress"])?;
        let mut candidates = Vec::new();

        for sha in output.lines().filter_map(parse_unreachable_commit_sha) {
            let subject = self
                .run_checked(["show", "-s", "--format=%s", sha.as_str()])?
                .trim()
                .to_string();
            if subject.starts_with("WIP on ") || subject.starts_with("On ") {
                candidates.push(StashEntry {
                    reference: sha.clone(),
                    commit: sha,
                    summary: subject,
                });
            }
        }

        Ok(candidates)
    }

    pub fn resolve_ref(&self, reference: &str) -> Result<String> {
        self.run_checked(["rev-parse", "--verify", reference])
            .map(|output| output.trim().to_string())
    }

    pub fn merge_base_commit(&self, left: &str, right: &str) -> Result<String> {
        self.merge_base(left, right)
    }

    pub fn commits_between(&self, base: &str, head: &str) -> Result<Vec<BranchCommit>> {
        let range = format!("{base}..{head}");
        let output = self.run_checked_slice(&[
            "log",
            "--reverse",
            "--format=%H%x1f%s%x1f%b%x1e",
            range.as_str(),
        ])?;
        Ok(parse_branch_commits(&output))
    }

    pub fn upstream_branch(&self) -> Result<Option<String>> {
        match self.run_checked(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]) {
            Ok(output) => Ok(Some(output.trim().to_string())),
            Err(_) => Ok(None),
        }
    }

    pub fn create_snapshot_ref(&self, reference: &str, target: &str) -> Result<String> {
        self.run_checked_slice(&["update-ref", reference, target])
    }

    pub fn create_branch_at(&self, branch: &str, target: &str) -> Result<String> {
        self.run_checked_slice(&["branch", branch, target])
    }

    pub fn create_and_checkout_branch(&self, branch: &str, target: &str) -> Result<String> {
        self.run_checked_slice(&["checkout", "-b", branch, target])
    }

    pub fn checkout_branch(&self, branch: &str) -> Result<String> {
        self.run_checked(["checkout", branch])
    }

    pub fn set_branch_ref(&self, branch: &str, target: &str) -> Result<String> {
        let reference = format!("refs/heads/{branch}");
        self.run_checked_slice(&["update-ref", reference.as_str(), target])
    }

    pub fn reset_hard(&self, target: &str) -> Result<String> {
        self.run_checked(["reset", "--hard", target])
    }

    pub fn rebase_abort(&self) -> Result<String> {
        self.run_checked(["rebase", "--abort"])
    }

    pub fn stash_branch(&self, branch: &str, stash_ref: &str) -> Result<String> {
        self.run_checked_slice(&["stash", "branch", branch, stash_ref])
    }

    pub fn stash_apply(&self, stash_ref: &str) -> Result<String> {
        self.run_checked(["stash", "apply", stash_ref])
    }

    pub fn fetch_remote_branch(&self, remote: &str, branch: &str) -> Result<String> {
        self.run_checked(["fetch", remote, branch])
    }

    pub fn force_push_ref(&self, remote: &str, source: &str, branch: &str) -> Result<String> {
        let spec = format!("{source}:refs/heads/{branch}");
        self.run_checked_slice(&["push", "--force-with-lease", remote, spec.as_str()])
    }

    pub fn run_suggested_command(&self, command: &str) -> Result<String> {
        let stripped = command
            .trim()
            .strip_prefix("git ")
            .ok_or_else(|| anyhow!("suggested command must start with 'git ': {}", command))?;

        let args: Vec<&str> = stripped.split_whitespace().collect();
        if args.is_empty() {
            bail!(
                "suggested command has no arguments after 'git': {}",
                command
            );
        }

        self.run_checked_slice(&args)
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

    fn ship_base(&self, plan: &PushPlan) -> Result<ShipBase> {
        match plan {
            PushPlan::Upstream { .. } => {
                let upstream = self.upstream_ref()?;
                Ok(ShipBase {
                    diff_base: upstream.clone(),
                    commit_range: Some(format!("{upstream}..HEAD")),
                    base_label: upstream,
                })
            }
            PushPlan::SetUpstream { remote, branch } => {
                let remote_branch_ref = format!("refs/remotes/{remote}/{branch}");
                if let Ok(output) =
                    self.run_checked_slice(&["rev-parse", "--verify", remote_branch_ref.as_str()])
                {
                    let remote_sha = output.trim().to_string();
                    return Ok(ShipBase {
                        diff_base: remote_sha.clone(),
                        commit_range: Some(format!("{remote_sha}..HEAD")),
                        base_label: format!("{remote}/{branch}"),
                    });
                }

                if let Some(default_ref) = self.remote_default_branch(remote)? {
                    let merge_base = self.merge_base("HEAD", default_ref.as_str())?;
                    return Ok(ShipBase {
                        diff_base: merge_base.clone(),
                        commit_range: Some(format!("{merge_base}..HEAD")),
                        base_label: shorten_remote_ref(&default_ref),
                    });
                }

                Ok(ShipBase {
                    diff_base: EMPTY_TREE_HASH.to_string(),
                    commit_range: None,
                    base_label: "repo start".to_string(),
                })
            }
        }
    }

    fn ship_commits(&self, base: &ShipBase) -> Result<Vec<BranchCommit>> {
        let mut args = vec!["log", "--reverse", "--format=%H%x1f%s%x1f%b%x1e"];
        if let Some(range) = &base.commit_range {
            args.push(range.as_str());
        } else {
            args.push("HEAD");
        }

        let output = self.run_checked_slice(&args)?;
        Ok(parse_branch_commits(&output))
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

    fn upstream_ref(&self) -> Result<String> {
        self.run_checked(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .map(|output| output.trim().to_string())
    }

    fn remote_default_branch(&self, remote: &str) -> Result<Option<String>> {
        let remote_head = format!("refs/remotes/{remote}/HEAD");
        match self.run_checked_slice(&["symbolic-ref", remote_head.as_str()]) {
            Ok(output) => {
                let value = output.trim();
                if value.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(value.to_string()))
                }
            }
            Err(_) => Ok(None),
        }
    }

    fn merge_base(&self, left: &str, right: &str) -> Result<String> {
        self.run_checked(["merge-base", left, right])
            .map(|output| output.trim().to_string())
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

fn append_risky_path_warnings(
    changed_files: &[String],
    risky_paths: &[String],
    warnings: &mut Vec<UnsafeDiffWarning>,
) {
    if risky_paths.is_empty() {
        return;
    }
    let matched: Vec<&str> = changed_files
        .iter()
        .filter(|path| {
            risky_paths.iter().any(|rp| {
                let prefix = rp.trim_end_matches('/');
                path.starts_with(&format!("{prefix}/")) || path.as_str() == prefix
            })
        })
        .map(String::as_str)
        .collect();
    if !matched.is_empty() {
        let display = if matched.len() <= 3 {
            matched.join(", ")
        } else {
            format!("{} and {} more", matched[..3].join(", "), matched.len() - 3)
        };
        warnings.push(UnsafeDiffWarning {
            message: format!("frequently changed paths detected (review carefully): {display}"),
        });
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

fn parse_branch_commits(raw: &str) -> Vec<BranchCommit> {
    raw.split('\u{1e}')
        .filter_map(|record| {
            let trimmed = record.trim();
            if trimmed.is_empty() {
                return None;
            }

            let mut fields = trimmed.splitn(3, '\u{1f}');
            let sha = fields.next()?.trim().to_string();
            let subject = fields.next()?.trim().to_string();
            let body = fields.next().unwrap_or("").trim().to_string();

            Some(BranchCommit { sha, subject, body })
        })
        .collect()
}

fn parse_reflog_entries(raw: &str) -> Vec<ReflogEntry> {
    raw.split('\u{1e}')
        .filter_map(|record| {
            let trimmed = record.trim();
            if trimmed.is_empty() {
                return None;
            }

            let mut fields = trimmed.splitn(3, '\u{1f}');
            Some(ReflogEntry {
                selector: fields.next()?.trim().to_string(),
                commit: fields.next()?.trim().to_string(),
                summary: fields.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect()
}

fn parse_stash_entries(raw: &str) -> Vec<StashEntry> {
    raw.split('\u{1e}')
        .filter_map(|record| {
            let trimmed = record.trim();
            if trimmed.is_empty() {
                return None;
            }

            let mut fields = trimmed.splitn(3, '\u{1f}');
            Some(StashEntry {
                reference: fields.next()?.trim().to_string(),
                commit: fields.next()?.trim().to_string(),
                summary: fields.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect()
}

fn parse_unreachable_commit_sha(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let sha = trimmed.strip_prefix("unreachable commit ")?;
    if sha.is_empty() {
        None
    } else {
        Some(sha.to_string())
    }
}

fn shorten_remote_ref(raw: &str) -> String {
    raw.trim()
        .strip_prefix("refs/remotes/")
        .unwrap_or(raw.trim())
        .to_string()
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
        is_lockfile_only_change, looks_minified_line, parse_branch_commits, parse_status_entry,
        parse_tracking, push_needs_force_with_lease, resolve_push_plan, shorten_remote_ref,
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

    #[test]
    fn parses_branch_commit_log_records() {
        let commits = parse_branch_commits(
            "abc123\x1fAdd ship flow\x1fBody line\x1e\ndef456\x1fFix tests\x1f\x1e",
        );

        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "abc123");
        assert_eq!(commits[0].subject, "Add ship flow");
        assert_eq!(commits[0].body, "Body line");
        assert_eq!(commits[1].subject, "Fix tests");
    }

    #[test]
    fn strips_remote_ref_prefix_for_display() {
        assert_eq!(
            shorten_remote_ref("refs/remotes/origin/main"),
            "origin/main"
        );
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
