use std::{
    fmt, fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use clap::ValueEnum;

use crate::git::{BranchCommit, GitRepo, ReflogEntry, StashEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum RescueIncident {
    WrongBranch,
    DetachedHead,
    BadRebase,
    LostStash,
    AccidentalReset,
    ForcePush,
}

impl RescueIncident {
    pub fn slug(self) -> &'static str {
        match self {
            Self::WrongBranch => "wrong-branch",
            Self::DetachedHead => "detached-head",
            Self::BadRebase => "bad-rebase",
            Self::LostStash => "lost-stash",
            Self::AccidentalReset => "accidental-reset",
            Self::ForcePush => "force-push",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::WrongBranch => "Wrong-Branch Commits",
            Self::DetachedHead => "Detached HEAD",
            Self::BadRebase => "Bad Rebase",
            Self::LostStash => "Lost Stash",
            Self::AccidentalReset => "Accidental Reset",
            Self::ForcePush => "Force-Push Mistake",
        }
    }

    pub fn all() -> [Self; 6] {
        [
            Self::WrongBranch,
            Self::DetachedHead,
            Self::BadRebase,
            Self::LostStash,
            Self::AccidentalReset,
            Self::ForcePush,
        ]
    }
}

impl fmt::Display for RescueIncident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueIncidentMatch {
    pub incident: RescueIncident,
    pub score: u8,
    pub summary: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueContext {
    pub branch: Option<String>,
    pub head_sha: String,
    pub upstream: Option<String>,
    pub default_branch: Option<String>,
    pub has_rebase_in_progress: bool,
    pub stash_entries: Vec<StashEntry>,
    pub lost_stash_candidates: Vec<StashEntry>,
    pub incident_matches: Vec<RescueIncidentMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescuePlan {
    pub incident: RescueIncident,
    pub title: String,
    pub summary: String,
    pub findings: Vec<String>,
    pub recommended: RescueOption,
    pub alternatives: Vec<RescueOption>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueOption {
    pub title: String,
    pub summary: String,
    pub impact: String,
    pub rollback_hint: String,
    pub steps: Vec<RescueStep>,
    pub requires_fetch: bool,
}

impl RescueOption {
    pub fn is_executable(&self) -> bool {
        !self.steps.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueStep {
    pub description: String,
    pub command_preview: String,
    pub action: RescueAction,
    pub mutates_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RescueAction {
    CreateAndCheckoutBranch {
        branch: String,
        target: String,
    },
    CreateBranch {
        branch: String,
        target: String,
    },
    SetBranchRef {
        branch: String,
        target: String,
    },
    ResetHard {
        target: String,
    },
    RebaseAbort,
    StashBranch {
        branch: String,
        stash_ref: String,
    },
    StashApply {
        stash_ref: String,
    },
    FetchRemoteBranch {
        remote: String,
        branch: String,
    },
    ForcePushRestore {
        remote: String,
        branch: String,
        source: RescueRefInput,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RescueRefInput {
    Fixed(String),
    Prompt {
        prompt: String,
        initial: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueSnapshot {
    pub reference: String,
    pub target: String,
    pub previous_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackNote {
    pub path: PathBuf,
    pub body: String,
    pub undo_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescueExecutionOutcome {
    pub snapshot: Option<RescueSnapshot>,
    pub executed_commands: Vec<String>,
    pub rollback: RollbackNote,
    pub summary: Vec<String>,
}

pub fn inspect_rescue_context(repo: &GitRepo) -> Result<RescueContext> {
    let branch = repo.branch_name()?;
    let head_sha = repo.head_sha()?;
    let upstream = repo.upstream_branch()?;
    let has_rebase_in_progress = repo.rebase_in_progress()?;
    let stash_entries = repo.stash_entries()?;
    let lost_stash_candidates = repo.dropped_stash_candidates()?;
    let default_branch = detect_default_branch(repo)?;
    let head_reflog = repo.reflog("HEAD", 20).unwrap_or_default();
    let matches = RescueIncident::all()
        .into_iter()
        .map(|incident| {
            match_incident(
                repo,
                incident,
                &branch,
                &upstream,
                &default_branch,
                &head_reflog,
                &stash_entries,
                &lost_stash_candidates,
                has_rebase_in_progress,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(RescueContext {
        branch,
        head_sha,
        upstream,
        default_branch,
        has_rebase_in_progress,
        stash_entries,
        lost_stash_candidates,
        incident_matches: matches,
    })
}

pub fn suggested_incident(context: &RescueContext) -> RescueIncident {
    context
        .incident_matches
        .iter()
        .max_by_key(|entry| entry.score)
        .map(|entry| entry.incident)
        .unwrap_or(RescueIncident::WrongBranch)
}

pub fn build_rescue_plan(
    repo: &GitRepo,
    context: &RescueContext,
    incident: RescueIncident,
) -> Result<RescuePlan> {
    match incident {
        RescueIncident::WrongBranch => build_wrong_branch_plan(repo, context),
        RescueIncident::DetachedHead => build_detached_head_plan(context),
        RescueIncident::BadRebase => build_bad_rebase_plan(repo, context),
        RescueIncident::LostStash => build_lost_stash_plan(context),
        RescueIncident::AccidentalReset => build_accidental_reset_plan(repo, context),
        RescueIncident::ForcePush => build_force_push_plan(repo, context),
    }
}

pub fn create_snapshot_ref_name(incident: RescueIncident) -> String {
    format!(
        "refs/gitgud/rescue/{}-{}",
        unix_timestamp(),
        incident.slug()
    )
}

pub fn execute_option(
    repo: &GitRepo,
    plan: &RescuePlan,
    option: &RescueOption,
    manual_ref: Option<String>,
) -> Result<RescueExecutionOutcome> {
    if option.steps.is_empty() {
        bail!("this rescue option has no executable steps");
    }

    let snapshot = if option.steps.iter().any(|step| step.mutates_repo) {
        Some(create_snapshot(repo, plan.incident)?)
    } else {
        None
    };

    let mut executed_commands = Vec::with_capacity(option.steps.len());
    for step in &option.steps {
        executed_commands.push(render_step_command(step, manual_ref.as_deref())?);
        match &step.action {
            RescueAction::CreateAndCheckoutBranch { branch, target } => {
                repo.create_and_checkout_branch(branch, target)?;
            }
            RescueAction::CreateBranch { branch, target } => {
                repo.create_branch_at(branch, target)?;
            }
            RescueAction::SetBranchRef { branch, target } => {
                repo.set_branch_ref(branch, target)?;
            }
            RescueAction::ResetHard { target } => {
                repo.reset_hard(target)?;
            }
            RescueAction::RebaseAbort => {
                repo.rebase_abort()?;
            }
            RescueAction::StashBranch { branch, stash_ref } => {
                repo.stash_branch(branch, stash_ref)?;
            }
            RescueAction::StashApply { stash_ref } => {
                repo.stash_apply(stash_ref)?;
            }
            RescueAction::FetchRemoteBranch { remote, branch } => {
                repo.fetch_remote_branch(remote, branch)?;
            }
            RescueAction::ForcePushRestore {
                remote,
                branch,
                source,
            } => {
                let source = resolve_ref_input(source, manual_ref.as_deref())?;
                repo.force_push_ref(remote, source, branch)?;
            }
        }
    }

    let rollback = save_rollback_note(repo, plan, option, snapshot.as_ref(), &executed_commands)?;
    let mut summary = vec![
        format!("Incident: {}", plan.incident.title()),
        format!("Applied: {}", option.title),
        format!("Rollback notes: {}", rollback.path.display()),
    ];
    if let Some(snapshot) = &snapshot {
        summary.push(format!("Snapshot ref: {}", snapshot.reference));
    }

    Ok(RescueExecutionOutcome {
        snapshot,
        executed_commands,
        rollback,
        summary,
    })
}

pub fn option_requires_manual_ref(option: &RescueOption) -> bool {
    option.steps.iter().any(|step| {
        matches!(
            step.action,
            RescueAction::ForcePushRestore {
                source: RescueRefInput::Prompt { .. },
                ..
            }
        )
    })
}

pub fn manual_ref_prompt(option: &RescueOption) -> Option<(String, Option<String>)> {
    option.steps.iter().find_map(|step| {
        if let RescueAction::ForcePushRestore {
            source: RescueRefInput::Prompt { prompt, initial },
            ..
        } = &step.action
        {
            Some((prompt.clone(), initial.clone()))
        } else {
            None
        }
    })
}

fn match_incident(
    repo: &GitRepo,
    incident: RescueIncident,
    branch: &Option<String>,
    upstream: &Option<String>,
    default_branch: &Option<String>,
    head_reflog: &[ReflogEntry],
    stash_entries: &[StashEntry],
    lost_stash_candidates: &[StashEntry],
    has_rebase_in_progress: bool,
) -> Result<RescueIncidentMatch> {
    let match_info = match incident {
        RescueIncident::WrongBranch => match_wrong_branch(repo, branch, upstream, default_branch)?,
        RescueIncident::DetachedHead => match_detached_head(branch),
        RescueIncident::BadRebase => match_bad_rebase(head_reflog, has_rebase_in_progress),
        RescueIncident::LostStash => match_lost_stash(stash_entries, lost_stash_candidates),
        RescueIncident::AccidentalReset => match_accidental_reset(head_reflog),
        RescueIncident::ForcePush => match_force_push(repo, upstream)?,
    };

    Ok(RescueIncidentMatch {
        incident,
        score: match_info.0,
        summary: match_info.1,
        evidence: match_info.2,
    })
}

fn build_wrong_branch_plan(repo: &GitRepo, context: &RescueContext) -> Result<RescuePlan> {
    let branch = context.branch.clone().unwrap_or_else(|| "HEAD".to_string());
    let branch_name = branch.clone();
    let parking_branch = recovery_branch_name(RescueIncident::WrongBranch, Some(&branch_name));
    let base_target = branch_base_target(repo, context)?;
    let commits = if let Some(base_target) = &base_target {
        repo.commits_between(base_target, "HEAD")
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let commit_note = if commits.is_empty() {
        "No ahead commits were detected, so rescue will park the current tip on a new branch."
            .to_string()
    } else {
        format!(
            "Recent commits on this tip: {}",
            summarize_commits(&commits)
        )
    };

    let mut findings = vec![format!("Current branch: {branch_name}"), commit_note];
    if let Some(base_target) = &base_target {
        findings.push(format!("Comparison base: {base_target}"));
    } else {
        findings.push(
            "No stable comparison base was found; rescue will avoid rewriting the original branch."
                .to_string(),
        );
    }

    let mut recommended_steps = vec![RescueStep {
        description: format!("Create and switch to {parking_branch} at the current tip"),
        command_preview: format!("git checkout -b {parking_branch} HEAD"),
        action: RescueAction::CreateAndCheckoutBranch {
            branch: parking_branch.clone(),
            target: "HEAD".to_string(),
        },
        mutates_repo: true,
    }];
    let mut recommended_summary =
        "Move the current tip onto a dedicated branch so the work is preserved first.".to_string();
    let mut recommended_impact =
        "Your current work stays reachable on a fresh branch before any cleanup happens."
            .to_string();

    if let Some(base_target) = &base_target {
        if branch_name == "main"
            || branch_name == "master"
            || context.default_branch.as_deref() == Some(branch_name.as_str())
        {
            recommended_steps.push(RescueStep {
                description: format!("Move {branch_name} back to {base_target}"),
                command_preview: format!("git update-ref refs/heads/{branch_name} {base_target}"),
                action: RescueAction::SetBranchRef {
                    branch: branch_name.clone(),
                    target: base_target.clone(),
                },
                mutates_repo: true,
            });
            recommended_summary =
                "Preserve the tip on a fresh branch, then restore the original branch to its clean base."
                    .to_string();
            recommended_impact = format!(
                "{branch_name} stops pointing at the accidental commits, while the new branch keeps them intact."
            );
        }
    }

    let alternative = RescueOption {
        title: "Park the work without touching the original branch".to_string(),
        summary:
            "Create the rescue branch and leave the current branch pointer where it is for manual cleanup later."
                .to_string(),
        impact: "This is safer if you want to inspect the branch history before rewriting anything."
            .to_string(),
        rollback_hint: format!("Delete {parking_branch} if you do not need the parked branch later."),
        steps: vec![RescueStep {
            description: format!("Create and switch to {parking_branch}"),
            command_preview: format!("git checkout -b {parking_branch} HEAD"),
            action: RescueAction::CreateAndCheckoutBranch {
                branch: parking_branch.clone(),
                target: "HEAD".to_string(),
            },
            mutates_repo: true,
        }],
        requires_fetch: false,
    };

    Ok(RescuePlan {
        incident: RescueIncident::WrongBranch,
        title: RescueIncident::WrongBranch.title().to_string(),
        summary: "The current tip looks like work that should live on its own branch.".to_string(),
        findings,
        recommended: RescueOption {
            title: "Move the work onto a rescue branch".to_string(),
            summary: recommended_summary,
            impact: recommended_impact,
            rollback_hint: format!("Use the snapshot ref to put {branch_name} back if needed."),
            steps: recommended_steps,
            requires_fetch: false,
        },
        alternatives: vec![alternative],
        note: Some(
            "This flow preserves the accidental commits before touching the original branch."
                .to_string(),
        ),
    })
}

fn build_detached_head_plan(context: &RescueContext) -> Result<RescuePlan> {
    let rescue_branch = recovery_branch_name(RescueIncident::DetachedHead, None);
    let mut alternatives = Vec::new();
    alternatives.push(RescueOption {
        title: "Create the branch without switching".to_string(),
        summary: "Record the detached commit on a named branch and stay on the current commit."
            .to_string(),
        impact: "Useful if you want to inspect the detached state before changing the checkout."
            .to_string(),
        rollback_hint: format!("Delete {rescue_branch} if you do not need the saved pointer."),
        steps: vec![RescueStep {
            description: format!("Create {rescue_branch} at {}", short_sha(&context.head_sha)),
            command_preview: format!("git branch {rescue_branch} {}", context.head_sha),
            action: RescueAction::CreateBranch {
                branch: rescue_branch.clone(),
                target: context.head_sha.clone(),
            },
            mutates_repo: true,
        }],
        requires_fetch: false,
    });

    Ok(RescuePlan {
        incident: RescueIncident::DetachedHead,
        title: RescueIncident::DetachedHead.title().to_string(),
        summary: "HEAD is detached, so the current commit can become hard to find unless it gets a branch name.".to_string(),
        findings: vec![
            format!("Current commit: {}", context.head_sha),
            "No branch currently points at the checkout.".to_string(),
        ],
        recommended: RescueOption {
            title: "Create and switch to a rescue branch".to_string(),
            summary: "Give the detached commit a durable branch name immediately.".to_string(),
            impact: "Your work becomes reachable through a normal branch again.".to_string(),
            rollback_hint: "Use the snapshot ref if you want to return to the detached state."
                .to_string(),
            steps: vec![RescueStep {
                description: format!("Create and switch to {rescue_branch}"),
                command_preview: format!("git checkout -b {rescue_branch} {}", context.head_sha),
                action: RescueAction::CreateAndCheckoutBranch {
                    branch: rescue_branch,
                    target: context.head_sha.clone(),
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        },
        alternatives,
        note: Some("Detached HEAD recovery is safest when the current commit gets a branch before any other cleanup.".to_string()),
    })
}

fn build_bad_rebase_plan(repo: &GitRepo, context: &RescueContext) -> Result<RescuePlan> {
    let head_reflog = repo.reflog("HEAD", 20).unwrap_or_default();
    if context.has_rebase_in_progress {
        let rescue_branch =
            recovery_branch_name(RescueIncident::BadRebase, context.branch.as_deref());
        return Ok(RescuePlan {
            incident: RescueIncident::BadRebase,
            title: RescueIncident::BadRebase.title().to_string(),
            summary: "A rebase is still in progress, so the safest recovery is to abort it before doing anything else.".to_string(),
            findings: vec![
                "Git reported an active rebase state.".to_string(),
                format!("Current commit during rebase: {}", context.head_sha),
            ],
            recommended: RescueOption {
                title: "Abort the in-progress rebase".to_string(),
                summary: "Return the branch and working tree to the pre-rebase state.".to_string(),
                impact: "This discards the partial rebase state but preserves the branch history from before the rebase started.".to_string(),
                rollback_hint: "Use the snapshot ref if you want to inspect the in-progress state again.".to_string(),
                steps: vec![RescueStep {
                    description: "Abort the rebase".to_string(),
                    command_preview: "git rebase --abort".to_string(),
                    action: RescueAction::RebaseAbort,
                    mutates_repo: true,
                }],
                requires_fetch: false,
            },
            alternatives: vec![RescueOption {
                title: "Save the current in-progress tip on a branch first".to_string(),
                summary: "Create a branch at the current commit, then abort the rebase.".to_string(),
                impact: "You keep a pointer to the in-progress state in case you want to inspect it later.".to_string(),
                rollback_hint: format!("Delete {rescue_branch} if you do not need the saved tip."),
                steps: vec![
                    RescueStep {
                        description: format!("Create {rescue_branch} at the current commit"),
                        command_preview: format!("git branch {rescue_branch} {}", context.head_sha),
                        action: RescueAction::CreateBranch {
                            branch: rescue_branch,
                            target: context.head_sha.clone(),
                        },
                        mutates_repo: true,
                    },
                    RescueStep {
                        description: "Abort the rebase".to_string(),
                        command_preview: "git rebase --abort".to_string(),
                        action: RescueAction::RebaseAbort,
                        mutates_repo: true,
                    },
                ],
                requires_fetch: false,
            }],
            note: Some("Abort is the least invasive way to exit a broken rebase while preserving work.".to_string()),
        });
    }

    let restore_target = find_previous_commit_after_keyword(&head_reflog, "rebase")
        .or_else(|| head_reflog.get(1).map(|entry| entry.commit.clone()))
        .unwrap_or_else(|| context.head_sha.clone());
    let rescue_branch = recovery_branch_name(RescueIncident::BadRebase, context.branch.as_deref());

    Ok(RescuePlan {
        incident: RescueIncident::BadRebase,
        title: RescueIncident::BadRebase.title().to_string(),
        summary: "Recent reflog entries suggest the branch was rewritten by a rebase.".to_string(),
        findings: vec![
            format!("Current commit: {}", context.head_sha),
            format!("Best pre-rebase candidate: {restore_target}"),
        ],
        recommended: RescueOption {
            title: "Reset the current branch to the pre-rebase tip".to_string(),
            summary: "Restore the branch pointer to the last commit seen before the rebase rewrite.".to_string(),
            impact: "The branch returns to the pre-rebase commit while the snapshot ref keeps the rewritten tip reachable.".to_string(),
            rollback_hint: "Use the snapshot ref to restore the rewritten tip if the target was not the right one.".to_string(),
            steps: vec![RescueStep {
                description: format!("Reset the current branch to {restore_target}"),
                command_preview: format!("git reset --hard {restore_target}"),
                action: RescueAction::ResetHard {
                    target: restore_target.clone(),
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        },
        alternatives: vec![RescueOption {
            title: "Recover the pre-rebase state on a fresh branch".to_string(),
            summary: "Create and switch to a branch at the pre-rebase commit without rewriting the current branch.".to_string(),
            impact: "Safer if you want to compare the rewritten history against the old state side by side.".to_string(),
            rollback_hint: format!("Delete {rescue_branch} if you do not need the recovered branch."),
            steps: vec![RescueStep {
                description: format!("Create and switch to {rescue_branch} at {restore_target}"),
                command_preview: format!("git checkout -b {rescue_branch} {restore_target}"),
                action: RescueAction::CreateAndCheckoutBranch {
                    branch: rescue_branch,
                    target: restore_target,
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        }],
        note: Some("The reflog candidate comes from local branch history, not from the AI layer.".to_string()),
    })
}

fn build_lost_stash_plan(context: &RescueContext) -> Result<RescuePlan> {
    let preferred = context
        .stash_entries
        .first()
        .cloned()
        .or_else(|| context.lost_stash_candidates.first().cloned());
    let Some(stash) = preferred else {
        return Ok(RescuePlan {
            incident: RescueIncident::LostStash,
            title: RescueIncident::LostStash.title().to_string(),
            summary: "No stash entries or dropped-stash candidates were found in this repository."
                .to_string(),
            findings: vec![
                "Live stash list is empty.".to_string(),
                "No dropped stash candidates matched the usual stash commit messages.".to_string(),
            ],
            recommended: RescueOption {
                title: "No automated recovery path available".to_string(),
                summary: "Rescue needs a stash entry or dangling stash commit to restore."
                    .to_string(),
                impact: "Try another incident flow or inspect older clones for the missing work."
                    .to_string(),
                rollback_hint: "No commands will run in this state.".to_string(),
                steps: Vec::new(),
                requires_fetch: false,
            },
            alternatives: Vec::new(),
            note: Some(
                "Lost stash recovery depends on stash metadata that still exists locally."
                    .to_string(),
            ),
        });
    };
    let rescue_branch = recovery_branch_name(RescueIncident::LostStash, None);

    Ok(RescuePlan {
        incident: RescueIncident::LostStash,
        title: RescueIncident::LostStash.title().to_string(),
        summary: "A stash entry or dropped-stash commit is available for guided recovery.".to_string(),
        findings: vec![
            format!("Best candidate: {} ({})", stash.reference, stash.summary),
            format!("Live stash count: {}", context.stash_entries.len()),
            format!(
                "Dropped stash candidates: {}",
                context.lost_stash_candidates.len()
            ),
        ],
        recommended: RescueOption {
            title: "Restore the stash onto a new branch".to_string(),
            summary: "Recover the stash in isolation so the changes can be reviewed safely.".to_string(),
            impact: "This keeps stash recovery away from your current branch and preserves the stash contents on a named branch.".to_string(),
            rollback_hint: format!("Delete {rescue_branch} if you decide not to keep the recovered stash branch."),
            steps: vec![RescueStep {
                description: format!("Create {rescue_branch} from {}", stash.reference),
                command_preview: format!("git stash branch {rescue_branch} {}", stash.reference),
                action: RescueAction::StashBranch {
                    branch: rescue_branch.clone(),
                    stash_ref: stash.reference.clone(),
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        },
        alternatives: vec![RescueOption {
            title: "Apply the stash on top of the current checkout".to_string(),
            summary: "Bring the recovered stash back into the current working tree without creating a branch.".to_string(),
            impact: "Faster, but it can create conflicts on the current branch.".to_string(),
            rollback_hint: "Use the snapshot ref if the apply creates a state you want to discard.".to_string(),
            steps: vec![RescueStep {
                description: format!("Apply {}", stash.reference),
                command_preview: format!("git stash apply {}", stash.reference),
                action: RescueAction::StashApply {
                    stash_ref: stash.reference,
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        }],
        note: Some("Stash recovery uses Git objects that are still reachable locally.".to_string()),
    })
}

fn build_accidental_reset_plan(repo: &GitRepo, context: &RescueContext) -> Result<RescuePlan> {
    let head_reflog = repo.reflog("HEAD", 20).unwrap_or_default();
    let restore_target = find_previous_commit_after_keyword(&head_reflog, "reset")
        .or_else(|| head_reflog.get(1).map(|entry| entry.commit.clone()))
        .unwrap_or_else(|| context.head_sha.clone());
    let rescue_branch =
        recovery_branch_name(RescueIncident::AccidentalReset, context.branch.as_deref());

    Ok(RescuePlan {
        incident: RescueIncident::AccidentalReset,
        title: RescueIncident::AccidentalReset.title().to_string(),
        summary: "Recent reflog entries suggest HEAD moved because of a reset.".to_string(),
        findings: vec![
            format!("Current commit: {}", context.head_sha),
            format!("Best pre-reset candidate: {restore_target}"),
        ],
        recommended: RescueOption {
            title: "Reset the current branch back to the pre-reset commit".to_string(),
            summary: "Move the branch pointer back to the last commit seen before the reset.".to_string(),
            impact: "The branch returns to the earlier commit while the snapshot ref keeps the post-reset state reachable.".to_string(),
            rollback_hint: "Use the snapshot ref if you need to return to the post-reset state.".to_string(),
            steps: vec![RescueStep {
                description: format!("Reset the current branch to {restore_target}"),
                command_preview: format!("git reset --hard {restore_target}"),
                action: RescueAction::ResetHard {
                    target: restore_target.clone(),
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        },
        alternatives: vec![RescueOption {
            title: "Recover the pre-reset tip on a fresh branch".to_string(),
            summary: "Create and switch to a new branch at the commit seen before the reset.".to_string(),
            impact: "Safer if you want to inspect the recovered state before rewriting the current branch.".to_string(),
            rollback_hint: format!("Delete {rescue_branch} if you do not need the recovered branch."),
            steps: vec![RescueStep {
                description: format!("Create and switch to {rescue_branch}"),
                command_preview: format!("git checkout -b {rescue_branch} {restore_target}"),
                action: RescueAction::CreateAndCheckoutBranch {
                    branch: rescue_branch,
                    target: restore_target,
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        }],
        note: Some("Reset recovery is based on reflog history, so act before the reflog entry expires.".to_string()),
    })
}

fn build_force_push_plan(repo: &GitRepo, context: &RescueContext) -> Result<RescuePlan> {
    let upstream = context
        .upstream
        .clone()
        .unwrap_or_else(|| "origin/main".to_string());
    let (remote, branch) = split_upstream(&upstream);
    let remote_reflog = repo.reflog(&upstream, 10).unwrap_or_default();
    let head_reflog = repo.reflog("HEAD", 10).unwrap_or_default();
    let restore_target = remote_reflog
        .get(1)
        .map(|entry| entry.commit.clone())
        .or_else(|| find_previous_commit_after_keyword(&head_reflog, "push"))
        .or_else(|| head_reflog.get(1).map(|entry| entry.commit.clone()));
    let requires_fetch = remote_reflog.is_empty();

    let mut findings = vec![format!("Upstream target: {upstream}")];
    if let Some(target) = &restore_target {
        findings.push(format!("Best restore candidate: {target}"));
    } else {
        findings.push(
            "No previous remote tip was discovered automatically; manual SHA input is available."
                .to_string(),
        );
    }
    if requires_fetch {
        findings.push(
            "Remote-tracking reflog is empty locally, so a fetch can refresh remote comparison data."
                .to_string(),
        );
    }

    let recommended = if let Some(target) = restore_target.clone() {
        let mut steps = Vec::new();
        if requires_fetch {
            steps.push(RescueStep {
                description: format!("Fetch {remote}/{branch} before restoring"),
                command_preview: format!("git fetch {remote} {branch}"),
                action: RescueAction::FetchRemoteBranch {
                    remote: remote.clone(),
                    branch: branch.clone(),
                },
                mutates_repo: true,
            });
        }
        steps.push(RescueStep {
            description: format!("Restore {upstream} to {target}"),
            command_preview: format!(
                "git push --force-with-lease {remote} {target}:refs/heads/{branch}"
            ),
            action: RescueAction::ForcePushRestore {
                remote: remote.clone(),
                branch: branch.clone(),
                source: RescueRefInput::Fixed(target.clone()),
            },
            mutates_repo: true,
        });

        RescueOption {
            title: "Restore the remote branch to the last known good commit".to_string(),
            summary: "Force-push the branch back to the previous remote tip using `--force-with-lease`.".to_string(),
            impact: "This repairs the remote branch while still protecting against clobbering newer remote work.".to_string(),
            rollback_hint: "Use the recorded SHA or snapshot notes if you need to reverse the remote restore.".to_string(),
            steps,
            requires_fetch,
        }
    } else {
        RescueOption {
            title: "Provide the commit you want to restore manually".to_string(),
            summary: "Rescue could not find the old remote tip automatically, so it needs an explicit SHA or ref.".to_string(),
            impact: "You stay in control of the exact commit that gets pushed back to the remote.".to_string(),
            rollback_hint: "Double-check the SHA before restoring because this action rewrites the remote branch.".to_string(),
            steps: vec![RescueStep {
                description: format!("Restore {upstream} from a user-provided SHA or ref"),
                command_preview: format!(
                    "git push --force-with-lease {remote} <sha-or-ref>:refs/heads/{branch}"
                ),
                action: RescueAction::ForcePushRestore {
                    remote: remote.clone(),
                    branch: branch.clone(),
                    source: RescueRefInput::Prompt {
                        prompt: format!(
                            "Enter the commit SHA or ref that should replace {upstream}:"
                        ),
                        initial: None,
                    },
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        }
    };

    let manual_option = RescueOption {
        title: "Restore the remote branch from a manual SHA or ref".to_string(),
        summary: "Use a specific commit or ref if the automatic candidate is not the one you want.".to_string(),
        impact: "This is the fallback path when reflog evidence is incomplete or you know the correct commit already.".to_string(),
        rollback_hint: "Verify the SHA first; this action rewrites the remote branch.".to_string(),
        steps: vec![RescueStep {
            description: format!("Restore {upstream} from a supplied SHA or ref"),
            command_preview: format!(
                "git push --force-with-lease {remote} <sha-or-ref>:refs/heads/{branch}"
            ),
            action: RescueAction::ForcePushRestore {
                remote,
                branch,
                source: RescueRefInput::Prompt {
                    prompt: format!("Enter the commit SHA or ref that should replace {upstream}:"),
                    initial: restore_target,
                },
            },
            mutates_repo: true,
        }],
        requires_fetch: false,
    };

    Ok(RescuePlan {
        incident: RescueIncident::ForcePush,
        title: RescueIncident::ForcePush.title().to_string(),
        summary:
            "This flow restores a remote branch tip when a force-push moved it to the wrong commit."
                .to_string(),
        findings,
        recommended,
        alternatives: vec![manual_option],
        note: Some(
            "Remote recovery is still deterministic: rescue only pushes an explicit commit or ref."
                .to_string(),
        ),
    })
}

fn create_snapshot(repo: &GitRepo, incident: RescueIncident) -> Result<RescueSnapshot> {
    let target = repo.head_sha()?;
    let previous_branch = repo.branch_name()?;
    let reference = create_snapshot_ref_name(incident);
    repo.create_snapshot_ref(reference.as_str(), target.as_str())?;

    Ok(RescueSnapshot {
        reference,
        target,
        previous_branch,
    })
}

fn save_rollback_note(
    repo: &GitRepo,
    plan: &RescuePlan,
    option: &RescueOption,
    snapshot: Option<&RescueSnapshot>,
    executed_commands: &[String],
) -> Result<RollbackNote> {
    let git_dir = repo.git_dir()?;
    let notes_dir = git_dir.join("gitgud").join("rescue");
    fs::create_dir_all(&notes_dir)?;
    let path = notes_dir.join(format!("{}-{}.md", unix_timestamp(), plan.incident.slug()));
    let undo_commands = build_undo_commands(snapshot);
    let body = render_rollback_note(plan, option, snapshot, executed_commands, &undo_commands);
    fs::write(&path, &body)?;

    Ok(RollbackNote {
        path,
        body,
        undo_commands,
    })
}

fn render_rollback_note(
    plan: &RescuePlan,
    option: &RescueOption,
    snapshot: Option<&RescueSnapshot>,
    executed_commands: &[String],
    undo_commands: &[String],
) -> String {
    let mut lines = vec![
        format!("# gg rescue rollback note: {}", plan.incident.slug()),
        String::new(),
        format!("Incident: {}", plan.title),
        format!("Applied option: {}", option.title),
        format!("Summary: {}", option.summary),
    ];
    if let Some(snapshot) = snapshot {
        lines.push(format!("Snapshot ref: {}", snapshot.reference));
        lines.push(format!("Snapshot target: {}", snapshot.target));
    }
    lines.push(String::new());
    lines.push("## Executed Commands".to_string());
    for command in executed_commands {
        lines.push(format!("- `{command}`"));
    }
    lines.push(String::new());
    lines.push("## Undo".to_string());
    if undo_commands.is_empty() {
        lines.push("- No automatic undo commands were recorded.".to_string());
    } else {
        for command in undo_commands {
            lines.push(format!("- `{command}`"));
        }
    }
    lines.push(String::new());
    lines.push("## Notes".to_string());
    lines.push(format!("- {}", option.rollback_hint));
    if let Some(plan_note) = &plan.note {
        lines.push(format!("- {}", plan_note));
    }

    lines.join("\n")
}

fn build_undo_commands(snapshot: Option<&RescueSnapshot>) -> Vec<String> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };

    let mut commands = Vec::new();
    if let Some(branch) = &snapshot.previous_branch {
        commands.push(format!("git checkout {branch}"));
        commands.push(format!("git reset --hard {}", snapshot.reference));
    } else {
        commands.push(format!(
            "git checkout -b detached-restore {}",
            snapshot.reference
        ));
    }
    commands
}

fn render_step_command(step: &RescueStep, manual_ref: Option<&str>) -> Result<String> {
    match &step.action {
        RescueAction::ForcePushRestore { source, .. } => {
            let source = resolve_ref_input(source, manual_ref)?;
            Ok(step.command_preview.replace("<sha-or-ref>", source))
        }
        _ => Ok(step.command_preview.clone()),
    }
}

fn resolve_ref_input<'a>(
    source: &'a RescueRefInput,
    manual_ref: Option<&'a str>,
) -> Result<&'a str> {
    match source {
        RescueRefInput::Fixed(value) => Ok(value.as_str()),
        RescueRefInput::Prompt { .. } => manual_ref
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("a commit SHA or ref is required for this rescue option")
            }),
    }
}

fn match_wrong_branch(
    repo: &GitRepo,
    branch: &Option<String>,
    upstream: &Option<String>,
    default_branch: &Option<String>,
) -> Result<(u8, String, Vec<String>)> {
    let Some(branch) = branch.as_deref() else {
        return Ok((
            5,
            "Detached HEAD usually takes priority over wrong-branch recovery.".to_string(),
            vec!["HEAD is detached.".to_string()],
        ));
    };

    let mut score = 20;
    let mut evidence = vec![format!("Current branch: {branch}")];
    let base = if let Some(upstream) = upstream {
        Some(upstream.clone())
    } else if default_branch
        .as_deref()
        .is_some_and(|candidate| candidate != branch)
    {
        default_branch.clone()
    } else {
        None
    };

    if let Some(base) = base {
        let merge_base = repo.merge_base_commit("HEAD", base.as_str())?;
        let ahead = repo
            .commits_between(merge_base.as_str(), "HEAD")
            .unwrap_or_default();
        if !ahead.is_empty() {
            score = if branch == "main" || branch == "master" {
                84
            } else {
                48
            };
            evidence.push(format!(
                "{} commit(s) appear after the branch base.",
                ahead.len()
            ));
            evidence.push(format!("Comparison base: {base}"));
        }
    }

    Ok((
        score,
        "This branch tip looks like work that may belong on a dedicated feature branch."
            .to_string(),
        evidence,
    ))
}

fn match_detached_head(branch: &Option<String>) -> (u8, String, Vec<String>) {
    if branch.is_none() {
        (
            100,
            "HEAD is detached, so rescue should name the current commit before it gets lost."
                .to_string(),
            vec!["No current branch is checked out.".to_string()],
        )
    } else {
        (
            0,
            "HEAD is attached to a branch, so detached-head rescue is less likely.".to_string(),
            vec!["A branch checkout is active.".to_string()],
        )
    }
}

fn match_bad_rebase(
    head_reflog: &[ReflogEntry],
    has_rebase_in_progress: bool,
) -> (u8, String, Vec<String>) {
    if has_rebase_in_progress {
        return (
            95,
            "Git has an active rebase in progress.".to_string(),
            vec!["A rebase directory exists under .git.".to_string()],
        );
    }

    let has_rebase_entry = head_reflog
        .iter()
        .any(|entry| entry.summary.to_ascii_lowercase().contains("rebase"));
    if has_rebase_entry {
        (
            72,
            "Recent reflog entries mention a rebase rewrite.".to_string(),
            vec!["HEAD reflog contains rebase entries.".to_string()],
        )
    } else {
        (
            8,
            "No active or recent rebase activity was found.".to_string(),
            vec!["Reflog does not mention rebase.".to_string()],
        )
    }
}

fn match_lost_stash(
    stash_entries: &[StashEntry],
    lost_stash_candidates: &[StashEntry],
) -> (u8, String, Vec<String>) {
    if !stash_entries.is_empty() || !lost_stash_candidates.is_empty() {
        (
            52,
            "Stash data is available for recovery.".to_string(),
            vec![
                format!("Live stash entries: {}", stash_entries.len()),
                format!("Dropped stash candidates: {}", lost_stash_candidates.len()),
            ],
        )
    } else {
        (
            4,
            "No stash data was found locally.".to_string(),
            vec!["Stash list and dropped-stash scan are both empty.".to_string()],
        )
    }
}

fn match_accidental_reset(head_reflog: &[ReflogEntry]) -> (u8, String, Vec<String>) {
    let has_reset = head_reflog
        .iter()
        .any(|entry| entry.summary.to_ascii_lowercase().contains("reset"));
    if has_reset {
        (
            78,
            "Recent reflog entries show a reset moved HEAD.".to_string(),
            vec!["HEAD reflog contains reset entries.".to_string()],
        )
    } else {
        (
            6,
            "No recent reset entries were found.".to_string(),
            vec!["HEAD reflog does not mention reset.".to_string()],
        )
    }
}

fn match_force_push(
    repo: &GitRepo,
    upstream: &Option<String>,
) -> Result<(u8, String, Vec<String>)> {
    let Some(upstream) = upstream else {
        return Ok((
            10,
            "No upstream is configured, so remote branch rescue is harder to infer.".to_string(),
            vec!["Current branch has no upstream.".to_string()],
        ));
    };

    let remote_reflog = repo.reflog(upstream, 5).unwrap_or_default();
    let score = if remote_reflog.len() >= 2 { 58 } else { 24 };
    let mut evidence = vec![format!("Upstream: {upstream}")];
    evidence.push(format!(
        "Remote-tracking reflog entries: {}",
        remote_reflog.len()
    ));

    Ok((
        score,
        "Force-push recovery is available when rescue can target a known upstream ref.".to_string(),
        evidence,
    ))
}

fn detect_default_branch(repo: &GitRepo) -> Result<Option<String>> {
    let local = repo.local_branches()?;
    for candidate in ["main", "master"] {
        if local.iter().any(|branch| branch == candidate) {
            return Ok(Some(candidate.to_string()));
        }
    }

    let remote = repo.remote_branches()?;
    for candidate in ["main", "master"] {
        if remote
            .iter()
            .any(|branch| branch.ends_with(&format!("/{candidate}")))
        {
            return Ok(Some(candidate.to_string()));
        }
    }

    Ok(None)
}

fn branch_base_target(repo: &GitRepo, context: &RescueContext) -> Result<Option<String>> {
    if let Some(upstream) = &context.upstream {
        return Ok(Some(repo.merge_base_commit("HEAD", upstream.as_str())?));
    }

    if let (Some(default_branch), Some(current_branch)) =
        (context.default_branch.as_deref(), context.branch.as_deref())
    {
        if default_branch != current_branch {
            return Ok(Some(repo.merge_base_commit("HEAD", default_branch)?));
        }
    }

    Ok(None)
}

fn find_previous_commit_after_keyword(reflog: &[ReflogEntry], keyword: &str) -> Option<String> {
    let keyword = keyword.to_ascii_lowercase();
    let mut saw_keyword = false;

    for entry in reflog {
        let summary = entry.summary.to_ascii_lowercase();
        if summary.contains(&keyword) {
            saw_keyword = true;
            continue;
        }

        if saw_keyword {
            return Some(entry.commit.clone());
        }
    }

    None
}

fn split_upstream(upstream: &str) -> (String, String) {
    let trimmed = upstream.trim();
    if let Some((remote, branch)) = trimmed.split_once('/') {
        (remote.to_string(), branch.to_string())
    } else {
        ("origin".to_string(), trimmed.to_string())
    }
}

fn recovery_branch_name(incident: RescueIncident, suffix: Option<&str>) -> String {
    let mut name = format!("rescue/{}", incident.slug());
    if let Some(suffix) = suffix {
        name.push('-');
        name.push_str(&sanitize_branch_component(suffix));
    }
    name.push('-');
    name.push_str(&unix_timestamp().to_string());
    name
}

fn sanitize_branch_component(raw: &str) -> String {
    let mut rendered = raw
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch.to_ascii_lowercase(),
            '/' | '_' | '-' => '-',
            _ => '-',
        })
        .collect::<String>();
    while rendered.contains("--") {
        rendered = rendered.replace("--", "-");
    }
    rendered.trim_matches('-').to_string()
}

fn summarize_commits(commits: &[BranchCommit]) -> String {
    commits
        .iter()
        .take(3)
        .map(|commit| commit.subject.clone())
        .collect::<Vec<_>>()
        .join("; ")
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        RescueAction, RescueIncident, RescueOption, RescueRefInput, RescueStep,
        create_snapshot_ref_name, manual_ref_prompt, option_requires_manual_ref,
        render_step_command, sanitize_branch_component,
    };

    #[test]
    fn snapshot_refs_use_hidden_namespace() {
        let name = create_snapshot_ref_name(RescueIncident::DetachedHead);
        assert!(name.starts_with("refs/gitgud/rescue/"));
        assert!(name.ends_with("detached-head"));
    }

    #[test]
    fn branch_component_sanitizes_symbols() {
        assert_eq!(sanitize_branch_component("Main Branch!"), "main-branch");
        assert_eq!(
            sanitize_branch_component("feature/test_case"),
            "feature-test-case"
        );
    }

    #[test]
    fn manual_ref_detection_finds_prompt_actions() {
        let option = RescueOption {
            title: "Manual".into(),
            summary: String::new(),
            impact: String::new(),
            rollback_hint: String::new(),
            steps: vec![RescueStep {
                description: String::new(),
                command_preview: "git push --force-with-lease origin <sha-or-ref>:refs/heads/main"
                    .into(),
                action: RescueAction::ForcePushRestore {
                    remote: "origin".into(),
                    branch: "main".into(),
                    source: RescueRefInput::Prompt {
                        prompt: "Enter a SHA".into(),
                        initial: Some("abc123".into()),
                    },
                },
                mutates_repo: true,
            }],
            requires_fetch: false,
        };

        assert!(option_requires_manual_ref(&option));
        assert_eq!(
            manual_ref_prompt(&option),
            Some(("Enter a SHA".into(), Some("abc123".into())))
        );
    }

    #[test]
    fn step_command_renders_manual_ref_value() {
        let step = RescueStep {
            description: String::new(),
            command_preview: "git push --force-with-lease origin <sha-or-ref>:refs/heads/main"
                .into(),
            action: RescueAction::ForcePushRestore {
                remote: "origin".into(),
                branch: "main".into(),
                source: RescueRefInput::Prompt {
                    prompt: "Enter a SHA".into(),
                    initial: None,
                },
            },
            mutates_repo: true,
        };

        let rendered = render_step_command(&step, Some("abc123")).unwrap();
        assert!(rendered.contains("abc123"));
    }
}
