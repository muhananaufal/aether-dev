//! Working out how to start a project, and on which port.
//!
//! The predecessor knew this: `php artisan serve` for Laravel, `npm run dev`
//! for a Vite project, and the port each of them lands on. That knowledge is
//! what turned a list of directories into something you could act on, so it
//! comes across - as defaults that can be replaced rather than as rules.
//!
//! Pure: the caller says which files a directory holds and what the
//! configuration overrides, and gets back a command. Nothing here runs it.

use serde::Deserialize;
use std::collections::HashMap;

/// A built-in way of starting one kind of project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub name: &'static str,
    pub command: &'static str,
    pub port: Option<u16>,
}

/// What the configuration says instead, for one recipe or one project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RunOverride {
    pub command: Option<String>,
    pub port: Option<u16>,
}

/// What will actually be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    /// The recipe this came from, or `None` when only an override named it.
    pub recipe: Option<&'static str>,
    pub command: String,
    pub port: Option<u16>,
}

/// The built-in recipes, most specific first.
///
/// Order is the whole design. Nearly every Laravel project carries a
/// package.json for its assets, and starting npm there runs a bundler instead
/// of the application - so the marker that means "this is the application" is
/// checked before the one that merely means "this has a frontend build".
const RECIPES: &[(&[&str], Recipe)] = &[
    (
        &["artisan"],
        Recipe {
            name: "laravel",
            command: "php artisan serve",
            port: Some(8000),
        },
    ),
    (
        &["spark"],
        Recipe {
            name: "codeigniter4",
            command: "php spark serve",
            port: Some(8080),
        },
    ),
    (
        &["manage.py"],
        Recipe {
            name: "django",
            command: "python manage.py runserver",
            port: Some(8000),
        },
    ),
    (
        &["next.config.js"],
        Recipe {
            name: "next",
            command: "npm run dev",
            port: Some(3000),
        },
    ),
    (
        &["next.config.ts"],
        Recipe {
            name: "next",
            command: "npm run dev",
            port: Some(3000),
        },
    ),
    (
        &["next.config.mjs"],
        Recipe {
            name: "next",
            command: "npm run dev",
            port: Some(3000),
        },
    ),
    (
        &["vite.config.js"],
        Recipe {
            name: "vite",
            command: "npm run dev",
            port: Some(5173),
        },
    ),
    (
        &["vite.config.ts"],
        Recipe {
            name: "vite",
            command: "npm run dev",
            port: Some(5173),
        },
    ),
    (
        &["main.py", "requirements.txt"],
        Recipe {
            name: "fastapi",
            command: "uvicorn main:app --reload",
            port: Some(8000),
        },
    ),
    (
        &["go.mod"],
        Recipe {
            name: "go",
            command: "go run .",
            port: Some(8080),
        },
    ),
    (
        &["Cargo.toml"],
        Recipe {
            name: "rust",
            command: "cargo run",
            port: Some(8080),
        },
    ),
    (
        &["index.php", "system"],
        Recipe {
            name: "codeigniter3",
            command: "php -S localhost:8000",
            port: Some(8000),
        },
    ),
    (
        &["package.json"],
        Recipe {
            name: "node",
            command: "npm start",
            port: Some(3000),
        },
    ),
    (
        &["requirements.txt"],
        Recipe {
            name: "python",
            command: "python app.py",
            port: Some(8000),
        },
    ),
];

/// Names the way this project starts, from the files it holds. A directory
/// that says nothing gets nothing rather than a guess.
pub fn detect(present: &[&str]) -> Option<Recipe> {
    RECIPES
        .iter()
        .find(|(markers, _)| markers.iter().all(|marker| present.contains(marker)))
        .map(|(_, recipe)| recipe.clone())
}

/// Decides what to run: the project's own entry first, then the recipe's, then
/// the built-in default.
///
/// A project name beats a recipe name because it is more specific - and
/// because a project can be called `laravel`, which is not hypothetical.
pub fn plan_for(
    project: &str,
    present: &[&str],
    by_recipe: &HashMap<String, RunOverride>,
    by_project: &HashMap<String, RunOverride>,
) -> Option<RunPlan> {
    let recipe = detect(present);
    let for_project = by_project.get(project);
    let for_recipe = recipe.as_ref().and_then(|found| by_recipe.get(found.name));

    let command = for_project
        .and_then(|over| over.command.clone())
        .or_else(|| for_recipe.and_then(|over| over.command.clone()))
        .or_else(|| recipe.as_ref().map(|found| found.command.to_string()))?;

    let port = for_project
        .and_then(|over| over.port)
        .or_else(|| for_recipe.and_then(|over| over.port))
        .or_else(|| recipe.as_ref().and_then(|found| found.port));

    Some(RunPlan {
        recipe: recipe.map(|found| found.name),
        command,
        port,
    })
}

/// Splits a command the way a shell would, so a configured command can carry
/// a quoted argument without it becoming two.
pub fn split(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;

    for character in command.chars() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(character);
                // An empty quoted string is still an argument.
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if started || !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(files: &[&str]) -> Option<Recipe> {
        detect(files)
    }

    #[test]
    fn laravel_is_recognised_by_its_own_entry_point() {
        let recipe = found(&["artisan", "composer.json", "package.json"]).expect("laravel");
        assert_eq!(recipe.name, "laravel");
        assert_eq!(recipe.command, "php artisan serve");
        assert_eq!(recipe.port, Some(8000));
    }

    #[test]
    fn a_laravel_project_is_not_mistaken_for_a_node_one() {
        // Nearly every Laravel project carries package.json for its assets, and
        // running npm there starts a bundler rather than the application.
        let recipe = found(&["package.json", "composer.json", "artisan"]).expect("laravel");
        assert_eq!(recipe.name, "laravel");
    }

    #[test]
    fn the_php_frameworks_are_told_apart_from_each_other() {
        assert_eq!(
            found(&["spark", "composer.json"]).unwrap().name,
            "codeigniter4"
        );
        assert_eq!(
            found(&["index.php", "system"]).unwrap().command,
            "php -S localhost:8000",
            "a codeigniter 3 project predates composer and has no runner of its own"
        );
    }

    #[test]
    fn the_javascript_frameworks_are_told_apart_by_their_config() {
        assert_eq!(
            found(&["next.config.js", "package.json"]).unwrap().name,
            "next"
        );
        assert_eq!(
            found(&["next.config.ts", "package.json"]).unwrap().port,
            Some(3000)
        );
        assert_eq!(
            found(&["vite.config.ts", "package.json"]).unwrap().port,
            Some(5173)
        );
        assert_eq!(
            found(&["package.json"]).unwrap().command,
            "npm start",
            "a plain node project has no dev script to assume"
        );
    }

    #[test]
    fn the_compiled_languages_run_themselves() {
        assert_eq!(found(&["go.mod"]).unwrap().command, "go run .");
        assert_eq!(found(&["Cargo.toml"]).unwrap().command, "cargo run");
    }

    #[test]
    fn python_projects_are_told_apart_by_what_they_are_built_with() {
        assert_eq!(
            found(&["manage.py"]).unwrap().command,
            "python manage.py runserver"
        );
        assert_eq!(
            found(&["main.py", "requirements.txt"]).unwrap().command,
            "uvicorn main:app --reload"
        );
        assert_eq!(
            found(&["requirements.txt"]).unwrap().command,
            "python app.py"
        );
    }

    #[test]
    fn a_directory_that_says_nothing_gets_no_command_rather_than_a_guess() {
        assert!(found(&["README.md", "notes.txt"]).is_none());
        assert!(found(&[]).is_none());
    }

    #[test]
    fn a_recipe_can_be_replaced_for_every_project_that_uses_it() {
        let mut recipes = HashMap::new();
        recipes.insert(
            "laravel".to_string(),
            RunOverride {
                command: Some("php artisan serve --host=0.0.0.0".to_string()),
                port: Some(8001),
            },
        );
        let plan = plan_for(
            "anything",
            &["artisan", "composer.json"],
            &recipes,
            &HashMap::new(),
        )
        .expect("a plan");
        assert_eq!(plan.command, "php artisan serve --host=0.0.0.0");
        assert_eq!(plan.port, Some(8001));
    }

    #[test]
    fn one_project_can_be_given_its_own_command_without_touching_the_others() {
        let mut projects = HashMap::new();
        projects.insert(
            "old-billing".to_string(),
            RunOverride {
                command: Some("php -S localhost:9000 -t public".to_string()),
                port: Some(9000),
            },
        );
        let mine = plan_for("old-billing", &["artisan"], &HashMap::new(), &projects).unwrap();
        assert_eq!(mine.command, "php -S localhost:9000 -t public");

        let other = plan_for("new-api", &["artisan"], &HashMap::new(), &projects).unwrap();
        assert_eq!(
            other.command, "php artisan serve",
            "the others are untouched"
        );
    }

    #[test]
    fn a_project_named_after_a_recipe_takes_its_own_entry_not_the_recipes() {
        // There is a project on this machine literally called "laravel", so
        // this collision is not hypothetical.
        let mut recipes = HashMap::new();
        recipes.insert(
            "laravel".to_string(),
            RunOverride {
                command: Some("recipe wins".to_string()),
                port: None,
            },
        );
        let mut projects = HashMap::new();
        projects.insert(
            "laravel".to_string(),
            RunOverride {
                command: Some("project wins".to_string()),
                port: None,
            },
        );
        let plan = plan_for("laravel", &["artisan"], &recipes, &projects).unwrap();
        assert_eq!(plan.command, "project wins");
    }

    #[test]
    fn an_override_that_only_changes_the_port_keeps_the_command() {
        let mut projects = HashMap::new();
        projects.insert(
            "shop".to_string(),
            RunOverride {
                command: None,
                port: Some(8123),
            },
        );
        let plan = plan_for("shop", &["artisan"], &HashMap::new(), &projects).unwrap();
        assert_eq!(plan.command, "php artisan serve");
        assert_eq!(plan.port, Some(8123));
    }

    #[test]
    fn a_project_with_no_recipe_can_still_be_given_a_command_by_hand() {
        let mut projects = HashMap::new();
        projects.insert(
            "odd-one".to_string(),
            RunOverride {
                command: Some("make serve".to_string()),
                port: Some(4000),
            },
        );
        let plan = plan_for("odd-one", &["README.md"], &HashMap::new(), &projects)
            .expect("an override alone is enough");
        assert_eq!(plan.command, "make serve");
        assert_eq!(plan.recipe, None, "there was no recipe to name");
    }

    #[test]
    fn a_command_is_split_the_way_a_shell_would_split_it() {
        assert_eq!(split("php artisan serve"), vec!["php", "artisan", "serve"]);
        assert_eq!(
            split("php -S localhost:8000 -t public"),
            vec!["php", "-S", "localhost:8000", "-t", "public"]
        );
        assert_eq!(
            split("npm run dev -- --host \"0.0.0.0\""),
            vec!["npm", "run", "dev", "--", "--host", "0.0.0.0"],
            "a quoted argument stays one argument"
        );
        assert!(split("   ").is_empty());
    }
}
