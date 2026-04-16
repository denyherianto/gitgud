use std::{fs, path::Path, process::Command};

use gitgud::ai::SplitCommitPlan;
use gitgud::git::{GitRepo, PushPlan};
use tempfile::TempDir;

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
    let temp = TempDir::new().unwrap();
    run(temp.path(), &["init", "-b", "main"]);
    run(temp.path(), &["config", "user.name", "Test User"]);
    run(temp.path(), &["config", "user.email", "test@example.com"]);
    fs::write(temp.path().join("README.md"), "hello\n").unwrap();
    run(temp.path(), &["add", "README.md"]);
    run(temp.path(), &["commit", "-m", "Initial commit"]);
    temp
}

#[test]
fn commits_staged_changes() {
    let repo_dir = init_repo();
    fs::write(repo_dir.path().join("README.md"), "hello\nworld\n").unwrap();
    run(repo_dir.path(), &["add", "README.md"]);

    let repo = GitRepo::new(repo_dir.path());
    let output = repo.commit("Update README").unwrap();
    assert!(output.contains("Update README"));

    let log = run(repo_dir.path(), &["log", "-1", "--pretty=%s"]);
    assert_eq!(log.trim(), "Update README");
}

#[test]
fn reports_no_staged_changes() {
    let repo_dir = init_repo();
    let repo = GitRepo::new(repo_dir.path());
    let staged = repo.staged_changes().unwrap();

    assert!(staged.staged_files.is_empty());
    assert!(staged.diff.trim().is_empty());
}

#[test]
fn stages_all_changes_when_requested() {
    let repo_dir = init_repo();
    fs::write(repo_dir.path().join("README.md"), "hello\nworld\n").unwrap();
    fs::write(repo_dir.path().join("notes.txt"), "draft\n").unwrap();

    let repo = GitRepo::new(repo_dir.path());
    repo.stage_all().unwrap();
    let staged = repo.staged_changes().unwrap();

    assert!(staged.staged_files.iter().any(|path| path == "README.md"));
    assert!(staged.staged_files.iter().any(|path| path == "notes.txt"));
}

#[test]
fn split_commit_creates_one_commit_per_plan() {
    let repo_dir = init_repo();
    fs::create_dir_all(repo_dir.path().join("src")).unwrap();
    fs::write(
        repo_dir.path().join("src/billing.rs"),
        "fn billing_summary_card() {}\n",
    )
    .unwrap();
    fs::write(
        repo_dir.path().join("src/subscription.rs"),
        "fn handle_subscription_status() {}\n",
    )
    .unwrap();
    run(
        repo_dir.path(),
        &["add", "src/billing.rs", "src/subscription.rs"],
    );

    let repo = GitRepo::new(repo_dir.path());
    repo.split_commit(&[
        SplitCommitPlan {
            message: "Add billing summary".into(),
            files: vec!["src/billing.rs".into()],
        },
        SplitCommitPlan {
            message: "Handle subscription status".into(),
            files: vec!["src/subscription.rs".into()],
        },
    ])
    .unwrap();

    let log = run(repo_dir.path(), &["log", "--pretty=%s", "-2"]);
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec!["Handle subscription status", "Add billing summary"]
    );

    let staged = repo.staged_changes().unwrap();
    assert!(staged.staged_files.is_empty());
}

#[test]
fn split_commit_rejects_plans_that_do_not_cover_all_staged_files() {
    let repo_dir = init_repo();
    fs::create_dir_all(repo_dir.path().join("src")).unwrap();
    fs::write(
        repo_dir.path().join("src/billing.rs"),
        "fn billing_summary_card() {}\n",
    )
    .unwrap();
    fs::write(
        repo_dir.path().join("src/subscription.rs"),
        "fn handle_subscription_status() {}\n",
    )
    .unwrap();
    run(
        repo_dir.path(),
        &["add", "src/billing.rs", "src/subscription.rs"],
    );

    let repo = GitRepo::new(repo_dir.path());
    let error = repo
        .split_commit(&[
            SplitCommitPlan {
                message: "Add billing summary".into(),
                files: vec!["src/billing.rs".into()],
            },
            SplitCommitPlan {
                message: "Duplicate billing summary".into(),
                files: vec!["src/billing.rs".into()],
            },
        ])
        .unwrap_err();

    assert!(error.to_string().contains("more than once"));

    let staged = repo.staged_changes().unwrap();
    assert_eq!(
        staged.staged_files,
        vec![
            "src/billing.rs".to_string(),
            "src/subscription.rs".to_string()
        ]
    );
}

#[test]
fn reports_status_counts_and_tracking() {
    let bare = TempDir::new().unwrap();
    run(bare.path(), &["init", "--bare"]);
    let repo_dir = init_repo();
    run(
        repo_dir.path(),
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    run(repo_dir.path(), &["push", "-u", "origin", "main"]);

    fs::write(repo_dir.path().join("README.md"), "hello\nworld\n").unwrap();
    fs::write(repo_dir.path().join("notes.txt"), "draft\n").unwrap();
    run(repo_dir.path(), &["add", "README.md"]);

    let repo = GitRepo::new(repo_dir.path());
    let status = repo.status().unwrap();

    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.staged_count, 1);
    assert_eq!(status.unstaged_count, 1);
    assert!(status.has_upstream);
    assert_eq!(status.tracking.as_deref(), Some("origin/main"));
    assert_eq!(status.staged_files, vec!["README.md".to_string()]);
    assert!(status.remotes.iter().any(|remote| remote == "origin"));
}

#[test]
fn plans_push_to_existing_upstream() {
    let bare = TempDir::new().unwrap();
    run(bare.path(), &["init", "--bare"]);
    let repo_dir = init_repo();
    run(
        repo_dir.path(),
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    run(repo_dir.path(), &["push", "-u", "origin", "main"]);

    let repo = GitRepo::new(repo_dir.path());
    let plan = repo.plan_push().unwrap();
    assert_eq!(
        plan,
        PushPlan::Upstream {
            branch: "main".into()
        }
    );
}

#[test]
fn plans_first_push_to_origin() {
    let bare = TempDir::new().unwrap();
    run(bare.path(), &["init", "--bare"]);
    let repo_dir = init_repo();
    run(
        repo_dir.path(),
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );

    let repo = GitRepo::new(repo_dir.path());
    let plan = repo.plan_push().unwrap();
    assert_eq!(
        plan,
        PushPlan::SetUpstream {
            remote: "origin".into(),
            branch: "main".into()
        }
    );
}

#[test]
fn plans_first_push_to_single_non_origin_remote() {
    let bare = TempDir::new().unwrap();
    run(bare.path(), &["init", "--bare"]);
    let repo_dir = init_repo();
    run(
        repo_dir.path(),
        &["remote", "add", "mirror", bare.path().to_str().unwrap()],
    );

    let repo = GitRepo::new(repo_dir.path());
    let plan = repo.plan_push().unwrap();
    assert_eq!(
        plan,
        PushPlan::SetUpstream {
            remote: "mirror".into(),
            branch: "main".into()
        }
    );
}

#[test]
fn rejects_ambiguous_first_push_when_multiple_remotes_exist() {
    let bare_a = TempDir::new().unwrap();
    let bare_b = TempDir::new().unwrap();
    run(bare_a.path(), &["init", "--bare"]);
    run(bare_b.path(), &["init", "--bare"]);
    let repo_dir = init_repo();
    run(
        repo_dir.path(),
        &["remote", "add", "mirror", bare_a.path().to_str().unwrap()],
    );
    run(
        repo_dir.path(),
        &["remote", "add", "backup", bare_b.path().to_str().unwrap()],
    );

    let repo = GitRepo::new(repo_dir.path());
    let error = repo.plan_push().unwrap_err();
    assert!(error.to_string().contains("ambiguous"));
}

#[test]
fn detects_detached_head_for_push() {
    let repo_dir = init_repo();
    let head = run(repo_dir.path(), &["rev-parse", "HEAD"]);
    run(repo_dir.path(), &["checkout", head.trim()]);

    let repo = GitRepo::new(repo_dir.path());
    let error = repo.plan_push().unwrap_err();
    assert!(error.to_string().contains("detached HEAD"));
}
