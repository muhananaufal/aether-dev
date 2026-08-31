//! The services this machine is meant to have, as opposed to the ones docker
//! happens to be running.
//!
//! Docker can only report containers that exist. A compose file usually
//! describes more than that — the ones nobody has started yet — and those are
//! exactly the ones somebody opens a dashboard to start. Declaring them is
//! also how the container name, the port, the hostname and the database
//! credentials stop being this tool's guesses and become the user's.

use crate::domain::{ServiceState, ServiceStatus};
use serde::Deserialize;

/// One service, as declared rather than as observed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServiceConfig {
    /// The container to look for. Without it the service's own name is used,
    /// which is what compose does when nothing overrides it.
    pub container: Option<String>,
    /// The port published to this machine. Worth declaring even when docker
    /// would report it: a stopped container reports none, and a row that loses
    /// its port the moment it stops is a row you cannot use to start it again.
    pub port: Option<u16>,
    /// A page to open for this service, if it has one.
    pub panel: Option<String>,
    /// The hostname `adev domains` should route to it.
    pub domain: Option<String>,
    /// The user a database dump should connect as, when it is not the one this
    /// tool would assume.
    pub user: Option<String>,
    /// Where the password lives in the container's environment, when it is not
    /// the variable the official image uses.
    pub password_env: Option<String>,
    /// The password itself, for a service whose container does not carry it.
    /// A last resort: it puts a secret in a file that gets copied around.
    pub password: Option<String>,
}

impl ServiceConfig {
    /// The container this service runs in.
    pub fn container_for(&self, name: &str) -> String {
        self.container.clone().unwrap_or_else(|| name.to_string())
    }
}

/// Puts the declared services and the observed containers together.
///
/// Declared ones come through even when nothing is running them, marked absent
/// rather than left out: a service you cannot see is a service you cannot
/// start. Containers nobody declared come through too, because docker knowing
/// about something the configuration does not is information, not noise.
pub fn merge(
    declared: &[(String, ServiceConfig)],
    observed: &[ServiceStatus],
) -> Vec<ServiceStatus> {
    let mut merged: Vec<ServiceStatus> = Vec::new();

    for (name, settings) in declared {
        let container = settings.container_for(name);
        match observed.iter().find(|seen| seen.container == container) {
            Some(seen) => merged.push(ServiceStatus {
                service: name.clone(),
                // The declared port wins only where docker has none to give:
                // a running container's published port is the truth.
                port: seen.port.or(settings.port),
                ..seen.clone()
            }),
            None => merged.push(ServiceStatus {
                container,
                service: name.clone(),
                port: settings.port,
                state: ServiceState::Absent,
                port_open: false,
                memory_bytes: None,
            }),
        }
    }

    for seen in observed {
        if !merged.iter().any(|have| have.container == seen.container) {
            merged.push(seen.clone());
        }
    }

    merged.sort_by(|a, b| a.service.cmp(&b.service));
    merged
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("service {service} names a domain but no port, so nothing could be routed to it")]
    DomainWithoutPort { service: String },
}

/// The routes the declared services ask for.
///
/// The upstream is the container rather than localhost: the proxy runs beside
/// them on docker's own network, where the container name is the address and
/// which port was published to this machine does not come into it.
pub fn service_domains(
    declared: &[(String, ServiceConfig)],
) -> Result<Vec<(String, String)>, CatalogError> {
    let mut routes = Vec::new();
    for (name, settings) in declared {
        let Some(host) = &settings.domain else {
            continue;
        };
        // A hostname with nothing behind it generates a route to nowhere,
        // which only fails later, at request time, with nothing to point at.
        let port = settings
            .port
            .ok_or_else(|| CatalogError::DomainWithoutPort {
                service: name.clone(),
            })?;
        routes.push((
            host.clone(),
            format!("{}:{port}", settings.container_for(name)),
        ));
    }
    Ok(routes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(entries: &[(&str, ServiceConfig)]) -> Vec<(String, ServiceConfig)> {
        entries
            .iter()
            .map(|(name, settings)| ((*name).to_string(), settings.clone()))
            .collect()
    }

    fn running(container: &str, service: &str, port: Option<u16>) -> ServiceStatus {
        ServiceStatus {
            container: container.to_string(),
            service: service.to_string(),
            port,
            state: ServiceState::Running,
            port_open: true,
            memory_bytes: None,
        }
    }

    #[test]
    fn a_service_nobody_has_started_is_still_listed() {
        let merged = merge(
            &declared(&[(
                "mongodb",
                ServiceConfig {
                    container: Some("mongo-db".to_string()),
                    port: Some(27017),
                    ..ServiceConfig::default()
                },
            )]),
            &[],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].state, ServiceState::Absent);
        assert_eq!(
            merged[0].port,
            Some(27017),
            "a service you cannot see is a service you cannot start"
        );
    }

    #[test]
    fn a_stopped_container_keeps_the_port_it_was_declared_with() {
        let stopped = ServiceStatus {
            state: ServiceState::Stopped,
            port_open: false,
            // Docker reports no ports for a container that is not running.
            ..running("pg", "postgres", None)
        };
        let merged = merge(
            &declared(&[(
                "postgres",
                ServiceConfig {
                    container: Some("pg".to_string()),
                    port: Some(5432),
                    ..ServiceConfig::default()
                },
            )]),
            &[stopped],
        );
        assert_eq!(
            merged[0].port,
            Some(5432),
            "a row that loses its port the moment it stops cannot be used to start it"
        );
    }

    #[test]
    fn a_running_container_keeps_the_port_it_actually_published() {
        let merged = merge(
            &declared(&[(
                "mysql",
                ServiceConfig {
                    container: Some("mysql-db".to_string()),
                    port: Some(3306),
                    ..ServiceConfig::default()
                },
            )]),
            &[running("mysql-db", "mysql", Some(33061))],
        );
        assert_eq!(
            merged[0].port,
            Some(33061),
            "what is actually published beats what somebody wrote down"
        );
    }

    #[test]
    fn a_container_nobody_declared_is_not_hidden() {
        let merged = merge(&[], &[running("stray", "stray", Some(9999))]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].service, "stray");
    }

    #[test]
    fn the_declared_name_is_the_one_shown() {
        let merged = merge(
            &declared(&[(
                "primary-database",
                ServiceConfig {
                    container: Some("mysql-db".to_string()),
                    ..ServiceConfig::default()
                },
            )]),
            &[running("mysql-db", "mysql", Some(3306))],
        );
        assert_eq!(merged.len(), 1, "the same container is not listed twice");
        assert_eq!(merged[0].service, "primary-database");
    }

    #[test]
    fn a_service_that_names_no_container_uses_its_own_name() {
        let settings = ServiceConfig::default();
        assert_eq!(settings.container_for("redis"), "redis");
        let named = ServiceConfig {
            container: Some("redis-db".to_string()),
            ..ServiceConfig::default()
        };
        assert_eq!(named.container_for("redis"), "redis-db");
    }

    #[test]
    fn a_service_that_names_a_domain_asks_for_a_route_to_its_container() {
        let routes = service_domains(&declared(&[(
            "dbgate",
            ServiceConfig {
                container: Some("dbgate-ui".to_string()),
                port: Some(19000),
                domain: Some("db.test".to_string()),
                ..ServiceConfig::default()
            },
        )]))
        .unwrap();
        assert_eq!(
            routes,
            vec![("db.test".to_string(), "dbgate-ui:19000".to_string())],
            "the proxy sits on docker's network, where the container is the address"
        );
    }

    #[test]
    fn a_service_with_no_domain_asks_for_no_route() {
        let routes = service_domains(&declared(&[(
            "mysql",
            ServiceConfig {
                port: Some(3306),
                ..ServiceConfig::default()
            },
        )]))
        .unwrap();
        assert!(routes.is_empty());
    }

    #[test]
    fn a_domain_with_no_port_behind_it_is_refused_by_name() {
        let err = service_domains(&declared(&[(
            "mailpit",
            ServiceConfig {
                domain: Some("mail.test".to_string()),
                ..ServiceConfig::default()
            },
        )]))
        .unwrap_err();
        assert!(
            err.to_string().contains("mailpit"),
            "a route to nowhere fails at request time, far from the config that caused it; \
             got {err}"
        );
    }

    #[test]
    fn services_come_back_in_a_stable_order() {
        let merged = merge(
            &declared(&[
                ("zeta", ServiceConfig::default()),
                ("alpha", ServiceConfig::default()),
            ]),
            &[running("middle", "middle", None)],
        );
        let names: Vec<&str> = merged.iter().map(|s| s.service.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zeta"]);
    }
}
