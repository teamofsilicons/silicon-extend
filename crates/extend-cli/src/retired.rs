//! Commands and flags Extend 3 had and Extend 4 doesn't (Teams, Silicon IAM, test environments).
//! Each is refused with exit 2, saying why it is gone and what to run instead, so a script or a
//! Silicon that still uses it learns the new way at once instead of getting "not a command".

use extend_protocol::ErrorCode;

use crate::error::CliError;

const NO_TEAMS: &str = "Devices aren't shared through groups any more: a device belongs to the Carbon who paired it, and a Silicon uses the devices it was given access to.";
const NO_TESTS: &str = "Extend 4 has no test environments.";
const ONE_SIGN_IN: &str = "A state directory holds one sign-in, of one account.";

/// `extend <words…>` when it names a removed command.
pub fn command(words: &[&str]) -> Option<CliError> {
    let gone = |what: &str, why: &str, instead: &str| {
        Some(
            CliError::new(
                ErrorCode::UnknownCommand,
                format!("`extend {what}` was removed in Extend 4. {why}"),
            )
            .hint(instead.to_owned()),
        )
    };
    match words {
        ["team", ..] => gone(
            "team",
            NO_TEAMS,
            "See the Silicons you look after and the ones you gave access to with `extend silicon ls`; give a Silicon access with `extend device access grant <device_id> <si:id>`.",
        ),
        ["permission", ..] => gone(
            "permission",
            "Extend asks for no separate approval: it stores the files a command makes in Briefcase with your own sign-in.",
            "Just run the command again; `extend file ls` lists the files.",
        ),
        ["env", ..] => gone(
            "env",
            NO_TESTS,
            "Develop against a local Extend and Silicon Accounts (EXTEND_API_URL and ACCOUNTS_URL), or install the development release: silicon-apps install 'extend>dev'.",
        ),
        ["config", "test", ..] => gone(
            "config test",
            NO_TESTS,
            "Develop against a local Extend and Silicon Accounts (EXTEND_API_URL and ACCOUNTS_URL), or install the development release: silicon-apps install 'extend>dev'.",
        ),
        ["login", "contexts" | "use", ..] => gone(
            &format!("login {}", words[1]),
            ONE_SIGN_IN,
            "For another account, use another state directory: SILICON_HOME=<dir> extend login, or `extend config home <dir>`.",
        ),
        ["device", "importable" | "import", ..] => gone(
            &format!("device {}", words[1]),
            "Devices aren't imported any more: every device you paired is yours.",
            "List them with `extend device ls`.",
        ),
        ["device", "visibility", ..] => gone(
            "device visibility",
            "Every device is private to the Carbon who paired it and the Silicons they give access to.",
            "Choose who can use it with `extend device access grant <device_id> <si:id>` and `extend device access revoke`.",
        ),
        _ => None,
    }
}

/// A removed global flag (`--team`, `--test`).
pub fn global_flag(flag: &str) -> CliError {
    match flag {
        "--team" => CliError::usage(
            format!("--team was removed in Extend 4. {NO_TEAMS}"),
            "Drop --team: `extend device ls` lists every device you can use or paired.",
        ),
        _ => CliError::usage(
            format!("--test was removed in Extend 4. {NO_TESTS} Nothing ran."),
            "Drop --test. To try changes, point EXTEND_API_URL and ACCOUNTS_URL at a local Extend and Silicon Accounts, or install the development release: silicon-apps install 'extend>dev'.",
        ),
    }
}

/// A removed flag of one command (`extend <path> <flag>`).
pub fn flag(path: &str, flag: &str) -> Option<CliError> {
    let removed = |why: &str, instead: &str| {
        Some(CliError::usage(
            format!("`extend {path}` no longer takes {flag}: {why}"),
            instead.to_owned(),
        ))
    };
    match (path, flag) {
        ("device ls", "--team-visible") => removed(
            NO_TEAMS,
            "Drop --team-visible: `extend device ls` lists every device you can use or paired.",
        ),
        ("device pair" | "device attach", "--visibility") => removed(
            "every device is private to the Carbon who paired it and the Silicons they give access to.",
            "Drop --visibility; give Silicons access with --access <si:id> (pair) or `extend device access grant`.",
        ),
        ("ting status" | "ting on", "--all-teams") => removed(
            "Extend's notifications reach an account through one registration.",
            "Drop --all-teams.",
        ),
        ("device wake-requests", "--only-team") => removed(
            "a Silicon has one grant per device.",
            "Drop --only-team; --silicon <si:id> alone mutes that Silicon on your pair.",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_retired_spelling_says_what_to_do_instead() {
        for words in [
            &["team", "ls"][..],
            &["team"],
            &["permission", "ls"],
            &["env", "show"],
            &["config", "test", "ls"],
            &["login", "contexts"],
            &["login", "use", "c:ada", "acme"],
            &["device", "import", "7c1e09ab"],
            &["device", "importable"],
            &["device", "visibility", "7c1e09ab", "team"],
        ] {
            let e = command(words).unwrap_or_else(|| panic!("{words:?} isn't refused"));
            assert_eq!(e.exit(), 2, "{words:?}");
            assert!(e.message.contains("removed in Extend 4"), "{}", e.message);
            assert!(
                e.hint
                    .as_deref()
                    .is_some_and(|h| h.contains("extend") || h.contains("silicon-apps")),
                "{words:?}"
            );
        }
        assert!(command(&["device", "ls"]).is_none() && command(&["login", "status"]).is_none());
        for f in ["--team", "--test"] {
            let e = global_flag(f);
            assert_eq!(e.exit(), 2);
            assert!(
                e.message.starts_with(&format!("{f} was removed in Extend 4")),
                "{}",
                e.message
            );
        }
        assert!(flag("device ls", "--team-visible").is_some());
        assert!(flag("device pair", "--visibility").is_some());
        assert!(flag("ting on", "--all-teams").is_some());
        assert!(flag("device wake-requests", "--only-team").is_some());
        assert!(flag("device ls", "--online").is_none());
    }
}
