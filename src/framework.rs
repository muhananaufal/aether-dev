//! Naming what a project is built on, and which version of it.
//!
//! Pure: the caller reads whichever files exist and passes their contents, so
//! the precedence rules and the version parsing are testable without a project
//! on disk.
//!
//! Where a framework ships its own version constant, that is preferred over
//! the constraint in a manifest. A constraint says what was asked for; the
//! installed source says what is actually there, and those differ often enough
//! to matter.

use serde::Deserialize;
use std::collections::HashMap;

/// The files a detector may consult. Absent ones are simply not read.
#[derive(Debug, Default)]
pub struct Manifests<'a> {
    pub composer_json: Option<&'a str>,
    /// `vendor/laravel/framework/src/Illuminate/Foundation/Application.php`,
    /// which carries the installed version as a constant.
    pub laravel_application_php: Option<&'a str>,
    pub package_json: Option<&'a str>,
    /// `system/core/CodeIgniter.php`, from before CodeIgniter used composer.
    pub codeigniter_php: Option<&'a str>,
    pub go_mod: Option<&'a str>,
    pub cargo_toml: Option<&'a str>,
}

#[derive(Deserialize, Default)]
struct ComposerJson {
    #[serde(default)]
    require: HashMap<String, String>,
    #[serde(default, rename = "require-dev")]
    require_dev: HashMap<String, String>,
}

#[derive(Deserialize, Default)]
struct PackageJson {
    #[serde(default)]
    dependencies: HashMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: HashMap<String, String>,
}

/// Names the framework and its version, or nothing when the project does not
/// say. Order is deliberate: nearly every Laravel project carries a
/// package.json for its assets, so PHP is answered before Node.
pub fn detect(manifests: &Manifests) -> Option<String> {
    if let Some(found) = php_framework(manifests) {
        return Some(found);
    }
    if let Some(source) = manifests.codeigniter_php {
        if let Some(version) = between(source, "CI_VERSION', '", "'") {
            return Some(format!("CodeIgniter {version}"));
        }
    }
    if let Some(found) = node_framework(manifests.package_json) {
        return Some(found);
    }
    if let Some(source) = manifests.go_mod {
        if let Some(version) = go_directive(source) {
            return Some(format!("Go {version}"));
        }
    }
    if let Some(source) = manifests.cargo_toml {
        if let Some(edition) = toml_string_value(source, "edition") {
            // The package version is what this project calls itself, which
            // says nothing about the stack it is built on.
            return Some(format!("Rust {edition}"));
        }
    }
    None
}

fn php_framework(manifests: &Manifests) -> Option<String> {
    if let Some(source) = manifests.laravel_application_php {
        if let Some(version) = between(source, "VERSION = '", "'") {
            return Some(format!("Laravel {version}"));
        }
    }

    let composer: ComposerJson = serde_json::from_str(manifests.composer_json?).ok()?;
    let requires = |package: &str| {
        composer
            .require
            .get(package)
            .or_else(|| composer.require_dev.get(package))
            .and_then(|constraint| loosen(constraint))
    };

    if let Some(version) = requires("laravel/framework") {
        return Some(format!("Laravel {version}"));
    }
    if let Some(version) = requires("laravel/lumen-framework") {
        return Some(format!("Lumen {version}"));
    }
    if let Some(version) = requires("codeigniter4/framework") {
        return Some(format!("CodeIgniter {version}"));
    }
    None
}

fn node_framework(package_json: Option<&str>) -> Option<String> {
    let package: PackageJson = serde_json::from_str(package_json?).ok()?;
    let depends_on = |name: &str| {
        package
            .dependencies
            .get(name)
            .or_else(|| package.dev_dependencies.get(name))
            .and_then(|constraint| loosen(constraint))
    };

    // A Next project also depends on react, so the more specific one answers.
    if let Some(version) = depends_on("next") {
        return Some(format!("Next.js {version}"));
    }
    if let Some(version) = depends_on("react") {
        return Some(format!("React {version}"));
    }
    if let Some(version) = depends_on("vue") {
        return Some(format!("Vue {version}"));
    }
    if let Some(version) = depends_on("svelte") {
        return Some(format!("Svelte {version}"));
    }
    None
}

/// Turns a constraint into the number inside it: `^11.0` and `>=9.2 <10` both
/// answer with the first version they mention. Nothing is invented - a
/// constraint with no number at all answers with nothing.
fn loosen(constraint: &str) -> Option<String> {
    let cleaned: String = constraint
        .chars()
        .map(|c| {
            if c.is_ascii_digit() || c == '.' {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|token| token.trim_matches('.').to_string())
}

/// The `go 1.24.3` line, which is the version the module asks for rather than
/// whatever toolchain happens to be installed.
fn go_directive(go_mod: &str) -> Option<String> {
    go_mod
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("go "))
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

fn toml_string_value(source: &str, key: &str) -> Option<String> {
    source.lines().map(str::trim).find_map(|line| {
        let rest = line.strip_prefix(key)?.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        Some(rest.trim_matches('"').to_string())
    })
}

fn between(source: &str, after: &str, before: &str) -> Option<String> {
    let start = source.find(after)? + after.len();
    let rest = &source[start..];
    let end = rest.find(before)?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing() -> Manifests<'static> {
        Manifests::default()
    }

    #[test]
    fn a_project_with_no_manifest_has_no_framework_rather_than_a_guess() {
        assert_eq!(detect(&nothing()), None);
    }

    #[test]
    fn laravel_prefers_the_version_actually_installed_over_the_one_asked_for() {
        let manifests = Manifests {
            composer_json: Some(r#"{"require":{"laravel/framework":"^10.0"}}"#),
            laravel_application_php: Some("class Application {\n    const VERSION = '11.9.2';\n}"),
            ..nothing()
        };
        assert_eq!(
            detect(&manifests).as_deref(),
            Some("Laravel 11.9.2"),
            "the constraint says what was asked for; vendor says what is there"
        );
    }

    #[test]
    fn laravel_falls_back_to_the_constraint_when_nothing_is_installed() {
        let manifests = Manifests {
            composer_json: Some(r#"{"require":{"laravel/framework":"^11.0"}}"#),
            ..nothing()
        };
        assert_eq!(detect(&manifests).as_deref(), Some("Laravel 11.0"));
    }

    #[test]
    fn constraint_markers_are_stripped_but_the_number_is_not_invented() {
        let manifests = Manifests {
            composer_json: Some(r#"{"require":{"laravel/framework":">=9.2 <10"}}"#),
            ..nothing()
        };
        assert_eq!(detect(&manifests).as_deref(), Some("Laravel 9.2"));
    }

    #[test]
    fn lumen_and_codeigniter_are_told_apart_from_laravel() {
        let lumen = Manifests {
            composer_json: Some(r#"{"require":{"laravel/lumen-framework":"^10.0"}}"#),
            ..nothing()
        };
        assert_eq!(detect(&lumen).as_deref(), Some("Lumen 10.0"));

        let ci4 = Manifests {
            composer_json: Some(r#"{"require":{"codeigniter4/framework":"^4.5"}}"#),
            ..nothing()
        };
        assert_eq!(detect(&ci4).as_deref(), Some("CodeIgniter 4.5"));
    }

    #[test]
    fn codeigniter_three_is_read_from_its_own_source_since_it_predates_composer() {
        let manifests = Manifests {
            codeigniter_php: Some("define('CI_VERSION', '3.1.13');"),
            ..nothing()
        };
        assert_eq!(detect(&manifests).as_deref(), Some("CodeIgniter 3.1.13"));
    }

    #[test]
    fn a_node_project_reports_the_framework_rather_than_the_runtime() {
        let next = Manifests {
            package_json: Some(r#"{"dependencies":{"next":"14.2.3","react":"18.3.1"}}"#),
            ..nothing()
        };
        assert_eq!(
            detect(&next).as_deref(),
            Some("Next.js 14.2.3"),
            "a Next project also depends on react; the more specific one is the answer"
        );

        let react = Manifests {
            package_json: Some(r#"{"devDependencies":{"react":"^18.3.1"}}"#),
            ..nothing()
        };
        assert_eq!(detect(&react).as_deref(), Some("React 18.3.1"));
    }

    #[test]
    fn go_reports_the_language_version_its_module_asks_for() {
        let manifests = Manifests {
            go_mod: Some("module example.com/thing\n\ngo 1.24.3\n\nrequire (\n)\n"),
            ..nothing()
        };
        assert_eq!(detect(&manifests).as_deref(), Some("Go 1.24.3"));
    }

    #[test]
    fn rust_reports_the_edition_because_the_package_version_is_the_projects_own() {
        let manifests = Manifests {
            cargo_toml: Some(
                "[package]\nname = \"thing\"\nversion = \"0.4.1\"\nedition = \"2021\"\n",
            ),
            ..nothing()
        };
        assert_eq!(
            detect(&manifests).as_deref(),
            Some("Rust 2021"),
            "0.4.1 is what this project calls itself, which says nothing about its stack"
        );
    }

    #[test]
    fn php_frameworks_outrank_a_package_json_that_is_only_there_for_the_build() {
        let manifests = Manifests {
            composer_json: Some(r#"{"require":{"laravel/framework":"^11.0"}}"#),
            package_json: Some(r#"{"devDependencies":{"react":"18.0.0"}}"#),
            ..nothing()
        };
        assert_eq!(
            detect(&manifests).as_deref(),
            Some("Laravel 11.0"),
            "nearly every Laravel project has a package.json for its assets"
        );
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_skipped_rather_than_fatal() {
        let manifests = Manifests {
            composer_json: Some("{ this is not json"),
            go_mod: Some("go 1.22.0\n"),
            ..nothing()
        };
        assert_eq!(detect(&manifests).as_deref(), Some("Go 1.22.0"));
    }
}
