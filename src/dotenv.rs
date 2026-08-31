//! Which `.env` a project is running with, and switching between them.
//!
//! A legacy project often carries `.env.local`, `.env.staging` and a couple of
//! others beside the `.env` that is actually read. The predecessor let you pick
//! one; this decides which is which, as a pure function, because the switch
//! itself overwrites a file and should not be guessing.

/// The files that can be switched to.
///
/// `.env` itself is excluded - it is the destination, not a choice - and so is
/// the backup, which exists only so a switch can be undone and would otherwise
/// offer itself as an option one switch later.
pub fn candidates(entries: &[String]) -> Vec<String> {
    let mut found: Vec<String> = entries
        .iter()
        .filter(|name| {
            name.starts_with(".env") && name.as_str() != ".env" && name.as_str() != BACKUP
        })
        .cloned()
        .collect();
    found.sort();
    found
}

/// The name of the backup a switch leaves behind.
pub const BACKUP: &str = ".env.bak";

/// Which candidate the active `.env` is a copy of, when it is a copy of one.
///
/// Compared by content rather than remembered in a state file: a state file
/// would go stale the moment somebody edited `.env` by hand, and then the tool
/// would confidently name the wrong one.
pub fn active(current: &str, candidates: &[(String, String)]) -> Option<String> {
    candidates
        .iter()
        .find(|(_, contents)| contents == current)
        .map(|(name, _)| name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(of: &[&str]) -> Vec<String> {
        of.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn the_files_that_can_be_switched_to_are_the_variants() {
        let found = candidates(&names(&[
            ".env",
            ".env.local",
            ".env.staging",
            ".env.bak",
            "composer.json",
            "artisan",
        ]));
        assert_eq!(found, vec![".env.local", ".env.staging"]);
    }

    #[test]
    fn the_active_file_is_not_offered_as_something_to_switch_to() {
        assert!(!candidates(&names(&[".env"])).contains(&".env".to_string()));
    }

    #[test]
    fn the_backup_is_not_offered_either() {
        assert!(
            !candidates(&names(&[".env.bak", ".env.local"])).contains(&".env.bak".to_string()),
            "it exists so a switch can be undone; offering it would make the \
             previous file look like a choice of its own"
        );
    }

    #[test]
    fn a_project_with_no_variants_offers_nothing_rather_than_erroring() {
        assert!(candidates(&names(&["composer.json", "artisan"])).is_empty());
    }

    #[test]
    fn which_one_is_active_is_decided_by_what_the_file_says() {
        let variants = vec![
            (".env.local".to_string(), "APP_ENV=local\n".to_string()),
            (".env.staging".to_string(), "APP_ENV=staging\n".to_string()),
        ];
        assert_eq!(
            active("APP_ENV=staging\n", &variants).as_deref(),
            Some(".env.staging")
        );
    }

    #[test]
    fn an_env_edited_by_hand_matches_nothing_rather_than_the_nearest() {
        let variants = vec![(".env.local".to_string(), "APP_ENV=local\n".to_string())];
        assert_eq!(
            active("APP_ENV=local\nDEBUG=1\n", &variants),
            None,
            "naming a file the contents no longer match would be worse than \
             saying nothing"
        );
    }

    #[test]
    fn nothing_to_compare_against_matches_nothing() {
        assert_eq!(active("APP_ENV=local\n", &[]), None);
    }
}
