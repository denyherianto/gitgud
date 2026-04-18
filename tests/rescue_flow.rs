use std::{fs, path::Path, process::Command};

use gitgud::git::GitRepo;
use gitgud::rescue::{
    RescueIncident, build_rescue_plan, execute_option, inspect_rescue_context, suggested_incident,
};
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

fn init_repo_with_remote() -> (TempDir, TempDir) {
    let bare = TempDir::new().unwrap();
    run(bare.path(), &["init", "--bare"]);
    let repo = init_repo();
    run(
        repo.path(),
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    run(repo.path(), &["push", "-u", "origin", "main"]);
    (bare, repo)
}

fn head_subject(dir: &Path) -> String {
    run(dir, &["log", "-1", "--pretty=%s"]).trim().to_string()
}

fn git_dir(dir: &Path) -> String {
    run(dir, &["rev-parse", "--git-dir"]).trim().to_string()
}

#[test]
fn detached_head_rescue_creates_branch_and_rollback_note() {
    let repo_dir = init_repo();
    let head = run(repo_dir.path(), &["rev-parse", "HEAD"]);
    run(repo_dir.path(), &["checkout", head.trim()]);

    let repo = GitRepo::new(repo_dir.path());
    let context = inspect_rescue_context(&repo).unwrap();
    assert_eq!(suggested_incident(&context), RescueIncident::DetachedHead);

    let plan = build_rescue_plan(&repo, &context, RescueIncident::DetachedHead).unwrap();
    let outcome = execute_option(&repo, &plan, &plan.recommended, None).unwrap();

    let branch = repo.branch_name().unwrap().unwrap();
    assert!(branch.starts_with("rescue/detached-head-"));
    assert!(
        outcome
            .rollback
            .path
            .to_string_lossy()
            .contains(".git/gitgud/rescue")
    );
}

#[test]
fn wrong_branch_rescue_moves_main_back_and_keeps_work_on_rescue_branch() {
    let (_bare, repo_dir) = init_repo_with_remote();
    fs::write(repo_dir.path().join("README.md"), "hello\nwrong branch\n").unwrap();
    run(repo_dir.path(), &["add", "README.md"]);
    run(
        repo_dir.path(),
        &["commit", "-m", "Work on main by mistake"],
    );
    let original_head = run(repo_dir.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let upstream_head = run(repo_dir.path(), &["rev-parse", "origin/main"])
        .trim()
        .to_string();

    let repo = GitRepo::new(repo_dir.path());
    let context = inspect_rescue_context(&repo).unwrap();
    let plan = build_rescue_plan(&repo, &context, RescueIncident::WrongBranch).unwrap();
    execute_option(&repo, &plan, &plan.recommended, None).unwrap();

    let current_branch = repo.branch_name().unwrap().unwrap();
    assert!(current_branch.starts_with("rescue/wrong-branch-main-"));
    assert_eq!(
        run(repo_dir.path(), &["rev-parse", "main"]).trim(),
        upstream_head
    );
    assert_eq!(
        run(repo_dir.path(), &["rev-parse", "HEAD"]).trim(),
        original_head
    );
}

#[test]
fn accidental_reset_rescue_restores_previous_commit() {
    let repo_dir = init_repo();
    fs::write(repo_dir.path().join("README.md"), "hello\nsecond\n").unwrap();
    run(repo_dir.path(), &["add", "README.md"]);
    run(repo_dir.path(), &["commit", "-m", "Second commit"]);
    run(repo_dir.path(), &["reset", "--hard", "HEAD~1"]);

    let repo = GitRepo::new(repo_dir.path());
    let context = inspect_rescue_context(&repo).unwrap();
    let plan = build_rescue_plan(&repo, &context, RescueIncident::AccidentalReset).unwrap();
    execute_option(&repo, &plan, &plan.recommended, None).unwrap();

    assert_eq!(head_subject(repo_dir.path()), "Second commit");
}

#[test]
fn lost_stash_rescue_restores_stash_on_new_branch() {
    let repo_dir = init_repo();
    fs::write(repo_dir.path().join("README.md"), "hello\nstashed\n").unwrap();
    run(repo_dir.path(), &["stash", "push", "-m", "save work"]);

    let repo = GitRepo::new(repo_dir.path());
    let context = inspect_rescue_context(&repo).unwrap();
    let plan = build_rescue_plan(&repo, &context, RescueIncident::LostStash).unwrap();
    execute_option(&repo, &plan, &plan.recommended, None).unwrap();

    let branch = repo.branch_name().unwrap().unwrap();
    assert!(branch.starts_with("rescue/lost-stash-"));
    let readme = fs::read_to_string(repo_dir.path().join("README.md")).unwrap();
    assert!(readme.contains("stashed"));
}

#[test]
fn bad_rebase_rescue_aborts_in_progress_rebase() {
    let repo_dir = init_repo();
    run(repo_dir.path(), &["checkout", "-b", "feature"]);
    fs::write(repo_dir.path().join("README.md"), "feature change\n").unwrap();
    run(repo_dir.path(), &["add", "README.md"]);
    run(repo_dir.path(), &["commit", "-m", "Feature change"]);

    run(repo_dir.path(), &["checkout", "main"]);
    fs::write(repo_dir.path().join("README.md"), "main change\n").unwrap();
    run(repo_dir.path(), &["add", "README.md"]);
    run(repo_dir.path(), &["commit", "-m", "Main change"]);

    run(repo_dir.path(), &["checkout", "feature"]);
    let output = Command::new("git")
        .current_dir(repo_dir.path())
        .args(["rebase", "main"])
        .output()
        .unwrap();
    assert!(!output.status.success());

    let repo = GitRepo::new(repo_dir.path());
    let context = inspect_rescue_context(&repo).unwrap();
    assert!(context.has_rebase_in_progress);
    let plan = build_rescue_plan(&repo, &context, RescueIncident::BadRebase).unwrap();
    execute_option(&repo, &plan, &plan.recommended, None).unwrap();

    assert!(!repo.rebase_in_progress().unwrap());
    assert_eq!(repo.branch_name().unwrap().as_deref(), Some("feature"));
}

#[test]
fn force_push_rescue_manual_sha_restores_remote_branch() {
    let (bare, repo_dir) = init_repo_with_remote();

    fs::write(repo_dir.path().join("README.md"), "hello\nstable\n").unwrap();
    run(repo_dir.path(), &["add", "README.md"]);
    run(repo_dir.path(), &["commit", "-m", "Stable remote commit"]);
    let good_sha = run(repo_dir.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    run(repo_dir.path(), &["push"]);

    let initial_sha = run(repo_dir.path(), &["rev-parse", "HEAD~1"])
        .trim()
        .to_string();
    run(
        repo_dir.path(),
        &["push", "--force", "origin", &format!("{initial_sha}:main")],
    );

    let repo = GitRepo::new(repo_dir.path());
    let context = inspect_rescue_context(&repo).unwrap();
    let plan = build_rescue_plan(&repo, &context, RescueIncident::ForcePush).unwrap();
    let manual = plan
        .alternatives
        .iter()
        .find(|option| option.title.contains("manual SHA"))
        .unwrap();
    execute_option(&repo, &plan, manual, Some(good_sha.clone())).unwrap();

    assert_eq!(run(bare.path(), &["rev-parse", "main"]).trim(), good_sha);
}

#[test]
fn rescue_writes_notes_under_hidden_git_directory() {
    let repo_dir = init_repo();
    let head = run(repo_dir.path(), &["rev-parse", "HEAD"]);
    run(repo_dir.path(), &["checkout", head.trim()]);

    let repo = GitRepo::new(repo_dir.path());
    let context = inspect_rescue_context(&repo).unwrap();
    let plan = build_rescue_plan(&repo, &context, RescueIncident::DetachedHead).unwrap();
    execute_option(&repo, &plan, &plan.recommended, None).unwrap();

    let notes_dir = repo_dir
        .path()
        .join(git_dir(repo_dir.path()))
        .join("gitgud")
        .join("rescue");
    let entries = fs::read_dir(notes_dir).unwrap().collect::<Vec<_>>();
    assert!(!entries.is_empty());
}
