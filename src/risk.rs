#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Safe,
    Medium,
    Dangerous,
}

pub fn classify_risk(command: &str) -> RiskLevel {
    let cmd = command.trim();
    let cmd = cmd.strip_prefix("git ").unwrap_or(cmd).trim();
    let tokens: Vec<&str> = cmd.split_whitespace().collect();

    let Some(&subcommand) = tokens.first() else {
        return RiskLevel::Safe;
    };

    let args = &tokens[1..];

    match subcommand {
        "reset" => {
            if args.iter().any(|a| *a == "--hard") {
                RiskLevel::Dangerous
            } else {
                RiskLevel::Medium
            }
        }
        "push" => {
            let has_force_with_lease = args.iter().any(|a| *a == "--force-with-lease");
            let has_force = args.iter().any(|a| *a == "--force" || *a == "-f");
            if has_force && !has_force_with_lease {
                RiskLevel::Dangerous
            } else if has_force_with_lease {
                RiskLevel::Medium
            } else {
                RiskLevel::Safe
            }
        }
        "clean" => {
            if args.iter().any(|a| a.starts_with('-') && a.contains('f')) {
                RiskLevel::Dangerous
            } else {
                RiskLevel::Medium
            }
        }
        "branch" => {
            if args.iter().any(|a| {
                *a == "-D" || (a.starts_with('-') && !a.starts_with("--") && a.contains('D'))
            }) {
                RiskLevel::Dangerous
            } else {
                RiskLevel::Safe
            }
        }
        "checkout" => {
            if args.iter().any(|a| *a == ".") {
                RiskLevel::Dangerous
            } else {
                RiskLevel::Safe
            }
        }
        "restore" => {
            if args.iter().any(|a| *a == ".") {
                RiskLevel::Dangerous
            } else {
                RiskLevel::Safe
            }
        }
        "rebase" => {
            let is_main_target = args.iter().any(|a| {
                matches!(*a, "main" | "master")
                    || a.ends_with("/main")
                    || a.ends_with("/master")
            });
            if is_main_target {
                RiskLevel::Dangerous
            } else {
                RiskLevel::Medium
            }
        }
        "stash" => {
            if args.first() == Some(&"list") {
                RiskLevel::Safe
            } else {
                RiskLevel::Medium
            }
        }
        "merge" | "cherry-pick" => RiskLevel::Medium,
        "pull" => {
            if args.iter().any(|a| *a == "--rebase") {
                RiskLevel::Medium
            } else {
                RiskLevel::Safe
            }
        }
        "commit" => {
            if args.iter().any(|a| *a == "--amend") {
                RiskLevel::Medium
            } else {
                RiskLevel::Safe
            }
        }
        "status" | "log" | "diff" | "show" | "blame" | "remote" | "tag" | "fetch" | "add" => {
            RiskLevel::Safe
        }
        _ => RiskLevel::Safe,
    }
}

#[cfg(test)]
mod tests {
    use super::{RiskLevel, classify_risk};

    #[test]
    fn classifies_reset_hard_as_dangerous() {
        assert_eq!(classify_risk("git reset --hard HEAD~1"), RiskLevel::Dangerous);
        assert_eq!(classify_risk("reset --hard"), RiskLevel::Dangerous);
    }

    #[test]
    fn classifies_reset_soft_and_mixed_as_medium() {
        assert_eq!(classify_risk("git reset --soft HEAD~1"), RiskLevel::Medium);
        assert_eq!(classify_risk("git reset HEAD~1"), RiskLevel::Medium);
    }

    #[test]
    fn classifies_force_push_as_dangerous() {
        assert_eq!(classify_risk("git push --force"), RiskLevel::Dangerous);
        assert_eq!(classify_risk("git push -f"), RiskLevel::Dangerous);
        assert_eq!(classify_risk("git push origin main --force"), RiskLevel::Dangerous);
    }

    #[test]
    fn classifies_force_with_lease_as_medium() {
        assert_eq!(classify_risk("git push --force-with-lease"), RiskLevel::Medium);
    }

    #[test]
    fn classifies_normal_push_as_safe() {
        assert_eq!(classify_risk("git push"), RiskLevel::Safe);
        assert_eq!(classify_risk("git push origin main"), RiskLevel::Safe);
    }

    #[test]
    fn classifies_clean_with_force_flag_as_dangerous() {
        assert_eq!(classify_risk("git clean -f"), RiskLevel::Dangerous);
        assert_eq!(classify_risk("git clean -fd"), RiskLevel::Dangerous);
    }

    #[test]
    fn classifies_branch_force_delete_as_dangerous() {
        assert_eq!(classify_risk("git branch -D feature-branch"), RiskLevel::Dangerous);
    }

    #[test]
    fn classifies_branch_soft_delete_as_safe() {
        assert_eq!(classify_risk("git branch -d feature-branch"), RiskLevel::Safe);
        assert_eq!(classify_risk("git branch --list"), RiskLevel::Safe);
    }

    #[test]
    fn classifies_checkout_dot_as_dangerous() {
        assert_eq!(classify_risk("git checkout ."), RiskLevel::Dangerous);
    }

    #[test]
    fn classifies_restore_dot_as_dangerous() {
        assert_eq!(classify_risk("git restore ."), RiskLevel::Dangerous);
    }

    #[test]
    fn classifies_rebase_main_as_dangerous() {
        assert_eq!(classify_risk("git rebase main"), RiskLevel::Dangerous);
        assert_eq!(classify_risk("git rebase master"), RiskLevel::Dangerous);
        assert_eq!(classify_risk("git rebase origin/main"), RiskLevel::Dangerous);
    }

    #[test]
    fn classifies_rebase_feature_as_medium() {
        assert_eq!(classify_risk("git rebase feature-branch"), RiskLevel::Medium);
        assert_eq!(classify_risk("git rebase -i HEAD~3"), RiskLevel::Medium);
    }

    #[test]
    fn classifies_stash_as_medium() {
        assert_eq!(classify_risk("git stash"), RiskLevel::Medium);
        assert_eq!(classify_risk("git stash drop"), RiskLevel::Medium);
    }

    #[test]
    fn classifies_stash_list_as_safe() {
        assert_eq!(classify_risk("git stash list"), RiskLevel::Safe);
    }

    #[test]
    fn classifies_merge_and_cherry_pick_as_medium() {
        assert_eq!(classify_risk("git merge feature-branch"), RiskLevel::Medium);
        assert_eq!(classify_risk("git cherry-pick abc1234"), RiskLevel::Medium);
    }

    #[test]
    fn classifies_pull_rebase_as_medium() {
        assert_eq!(classify_risk("git pull --rebase"), RiskLevel::Medium);
    }

    #[test]
    fn classifies_commit_amend_as_medium() {
        assert_eq!(classify_risk("git commit --amend"), RiskLevel::Medium);
    }

    #[test]
    fn classifies_safe_commands() {
        assert_eq!(classify_risk("git status"), RiskLevel::Safe);
        assert_eq!(classify_risk("git log --oneline -5"), RiskLevel::Safe);
        assert_eq!(classify_risk("git diff"), RiskLevel::Safe);
        assert_eq!(classify_risk("git show HEAD"), RiskLevel::Safe);
        assert_eq!(classify_risk("git blame src/main.rs"), RiskLevel::Safe);
        assert_eq!(classify_risk("git fetch"), RiskLevel::Safe);
        assert_eq!(classify_risk("git add ."), RiskLevel::Safe);
        assert_eq!(classify_risk("git tag v1.0"), RiskLevel::Safe);
        assert_eq!(classify_risk("git remote -v"), RiskLevel::Safe);
    }
}
