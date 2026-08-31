//! Talking to the Docker Engine API.
//!
//! Split deliberately: reading the daemon's answer is a pure function with a
//! fixture captured from a real daemon, and the transport is the thin part
//! around it. That way the field names are tested without needing Docker
//! running, and only the socket work depends on the machine.

use crate::domain::{ServiceState, ServiceStatus};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("no docker endpoint: set DOCKER_HOST, or docker.endpoint in the config file")]
    NoEndpoint,
    #[error("docker endpoint transport is not supported by this build: {0}")]
    UnsupportedTransport(String),
    #[error("docker returned something this build cannot read: {0}")]
    Malformed(String),
    #[error("cannot reach docker at {endpoint}: {reason}")]
    Unreachable { endpoint: String, reason: String },
}

pub trait DockerClient: Send + Sync {
    fn services(&self) -> Result<Vec<ServiceStatus>, DockerError>;
}

#[derive(Deserialize)]
struct ApiContainer {
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "Ports", default)]
    ports: Vec<ApiPort>,
    #[serde(rename = "Labels", default)]
    labels: HashMap<String, String>,
}

#[derive(Deserialize)]
struct ApiPort {
    #[serde(rename = "PublicPort")]
    public_port: Option<u16>,
    #[serde(rename = "Type", default)]
    port_type: String,
}

/// Turns the daemon's container list into service rows.
pub fn parse_containers(json: &str) -> Result<Vec<ServiceStatus>, DockerError> {
    let raw: Vec<ApiContainer> =
        serde_json::from_str(json).map_err(|error| DockerError::Malformed(error.to_string()))?;

    Ok(raw
        .into_iter()
        .map(|container| {
            let name = container
                .names
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_default();
            let service = container
                .labels
                .get("com.docker.compose.service")
                .cloned()
                .unwrap_or_else(|| name.clone());
            // Unpublished ports cannot be reached from the host and udp is not
            // how any of these services are spoken to. The lowest published
            // port wins because Docker does not promise an order, and a
            // service that reported 80 on one call and 443 on the next was
            // exactly the bug this replaces.
            let port = container
                .ports
                .iter()
                .filter(|p| p.port_type == "tcp")
                .filter_map(|p| p.public_port)
                .min();

            ServiceStatus {
                container: name,
                service,
                port,
                state: if container.state == "running" {
                    ServiceState::Running
                } else {
                    ServiceState::Stopped
                },
                // The daemon saying "running" is not the same as the port
                // answering; that is a separate question, asked separately.
                port_open: false,
                memory_bytes: None,
            }
        })
        .collect())
}

/// Resolves the configured endpoint into an HTTP base URL, following
/// `DOCKER_HOST` when the config says `auto` the way other docker clients do.
pub fn base_url(endpoint: &str, docker_host: Option<&str>) -> Result<String, DockerError> {
    let endpoint = if endpoint == "auto" {
        docker_host.ok_or(DockerError::NoEndpoint)?
    } else {
        endpoint
    };

    if let Some(host) = endpoint.strip_prefix("tcp://") {
        Ok(format!("http://{host}"))
    } else if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        Ok(endpoint.to_string())
    } else {
        Err(DockerError::UnsupportedTransport(endpoint.to_string()))
    }
}

/// Asks whether a port on this machine actually answers.
///
/// The daemon reporting a container as running says the process exists, not
/// that the service inside it is ready. A database container is up for
/// seconds before it accepts connections, and the tool this replaces showed
/// those seconds as ONLINE.
pub fn probe_port(port: u16, timeout: std::time::Duration) -> bool {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&address, timeout).is_ok()
}

/// Probes every service that publishes a port, all at once. Sequential probes
/// would add one timeout per stopped service to every refresh.
pub fn probe_all(services: &mut [ServiceStatus], timeout: std::time::Duration) {
    let probes: Vec<_> = services
        .iter()
        .map(|service| service.port)
        .map(|port| {
            std::thread::spawn(move || match port {
                Some(port) => probe_port(port, timeout),
                None => false,
            })
        })
        .collect();

    for (service, probe) in services.iter_mut().zip(probes) {
        service.port_open = probe.join().unwrap_or(false);
    }
}

/// Reads the Docker Engine API over HTTP.
pub struct HttpDockerClient {
    base: String,
}

impl HttpDockerClient {
    pub fn new(endpoint: &str, docker_host: Option<&str>) -> Result<Self, DockerError> {
        Ok(Self {
            base: base_url(endpoint, docker_host)?,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.base
    }
}

impl DockerClient for HttpDockerClient {
    fn services(&self) -> Result<Vec<ServiceStatus>, DockerError> {
        let url = format!("{}/containers/json?all=true", self.base);
        let unreachable = |reason: String| DockerError::Unreachable {
            endpoint: self.base.clone(),
            reason,
        };

        let body = ureq::get(&url)
            .call()
            .map_err(|error| unreachable(error.to_string()))?
            .body_mut()
            .read_to_string()
            .map_err(|error| unreachable(error.to_string()))?;

        parse_containers(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ServiceState;

    /// Trimmed from this machine's own daemon (API 1.55), so the field names
    /// and shapes are the real ones rather than a guess about Docker's format.
    const REAL_RESPONSE: &str = r#"[
      {
        "Names": ["/caddy-proxy"],
        "Image": "caddy:alpine",
        "State": "running",
        "Status": "Up 27 minutes",
        "Ports": [
          {"IP":"0.0.0.0","PrivatePort":80,"PublicPort":80,"Type":"tcp"},
          {"IP":"::","PrivatePort":80,"PublicPort":80,"Type":"tcp"},
          {"IP":"","PrivatePort":2019,"Type":"tcp"},
          {"IP":"","PrivatePort":443,"Type":"udp"}
        ],
        "Labels": {"com.docker.compose.service":"caddy","com.docker.compose.project":"db-stack"}
      },
      {
        "Names": ["/docker-tcp-proxy"],
        "Image": "alpine/socat",
        "State": "exited",
        "Status": "Exited (0) 2 minutes ago",
        "Ports": [],
        "Labels": {}
      }
    ]"#;

    #[test]
    fn a_service_takes_its_name_from_the_compose_label() {
        let services = parse_containers(REAL_RESPONSE).unwrap();
        assert_eq!(services[0].container, "caddy-proxy");
        assert_eq!(services[0].service, "caddy");
    }

    #[test]
    fn a_container_outside_compose_falls_back_to_its_own_name() {
        let services = parse_containers(REAL_RESPONSE).unwrap();
        assert_eq!(services[1].container, "docker-tcp-proxy");
        assert_eq!(services[1].service, "docker-tcp-proxy");
    }

    #[test]
    fn only_published_tcp_ports_count_as_the_service_port() {
        let services = parse_containers(REAL_RESPONSE).unwrap();
        assert_eq!(
            services[0].port,
            Some(80),
            "2019 is unpublished and 443 is udp; neither is how you reach this service"
        );
        assert_eq!(services[1].port, None);
    }

    #[test]
    fn a_state_other_than_running_is_stopped() {
        let services = parse_containers(REAL_RESPONSE).unwrap();
        assert_eq!(services[0].state, ServiceState::Running);
        assert_eq!(services[1].state, ServiceState::Stopped);
    }

    #[test]
    fn a_freshly_listed_service_is_not_yet_known_to_be_reachable() {
        let services = parse_containers(REAL_RESPONSE).unwrap();
        assert!(
            !services[0].port_open,
            "the daemon says running; only a connection says reachable"
        );
    }

    #[test]
    fn malformed_json_is_an_error_rather_than_an_empty_list() {
        let err = parse_containers("{not json").unwrap_err();
        assert!(matches!(err, DockerError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn a_tcp_endpoint_becomes_an_http_base_url() {
        assert_eq!(
            base_url("tcp://127.0.0.1:2375", None).unwrap(),
            "http://127.0.0.1:2375"
        );
        assert_eq!(
            base_url("http://localhost:2375", None).unwrap(),
            "http://localhost:2375"
        );
    }

    #[test]
    fn auto_follows_docker_host_the_way_every_other_docker_client_does() {
        assert_eq!(
            base_url("auto", Some("tcp://localhost:2375")).unwrap(),
            "http://localhost:2375"
        );
    }

    #[test]
    fn auto_without_docker_host_says_what_to_configure_instead_of_guessing() {
        let err = base_url("auto", None).unwrap_err();
        assert!(matches!(err, DockerError::NoEndpoint), "got {err:?}");
        assert!(err.to_string().contains("docker.endpoint"));
    }

    #[test]
    fn transports_that_are_not_built_yet_are_named_not_silently_ignored() {
        let err = base_url("unix:///var/run/docker.sock", None).unwrap_err();
        assert!(
            matches!(err, DockerError::UnsupportedTransport(_)),
            "got {err:?}"
        );
        let err = base_url("npipe:////./pipe/docker_engine", None).unwrap_err();
        assert!(
            matches!(err, DockerError::UnsupportedTransport(_)),
            "got {err:?}"
        );
    }
    #[test]
    fn a_port_with_something_listening_answers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(probe_port(port, std::time::Duration::from_millis(300)));
    }

    #[test]
    fn a_port_with_nothing_listening_does_not() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!probe_port(port, std::time::Duration::from_millis(300)));
    }

    #[test]
    fn running_plus_an_answering_port_is_what_reachable_means() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut services = parse_containers(REAL_RESPONSE).unwrap();
        services[0].port = Some(port);
        probe_all(&mut services, std::time::Duration::from_millis(300));
        assert!(services[0].is_reachable());
        assert!(
            !services[1].is_reachable(),
            "a stopped container has nothing to answer"
        );
    }

    /// Same container, ports listed in the other order. Docker does not
    /// promise an order, and the answer must not depend on one.
    const REVERSED_PORTS: &str = r#"[
      {
        "Names": ["/caddy-proxy"],
        "State": "running",
        "Ports": [
          {"IP":"0.0.0.0","PrivatePort":443,"PublicPort":443,"Type":"tcp"},
          {"IP":"0.0.0.0","PrivatePort":80,"PublicPort":80,"Type":"tcp"}
        ],
        "Labels": {"com.docker.compose.service":"caddy"}
      }
    ]"#;

    #[test]
    fn the_service_port_does_not_depend_on_the_order_docker_listed_them() {
        let forward = parse_containers(REAL_RESPONSE).unwrap()[0].port;
        let reversed = parse_containers(REVERSED_PORTS).unwrap()[0].port;
        assert_eq!(
            forward, reversed,
            "two calls to the same daemon reported different ports for one service"
        );
        assert_eq!(forward, Some(80));
    }
}
