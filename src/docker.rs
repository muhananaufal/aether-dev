//! Talking to the Docker Engine API.
//!
//! Split deliberately: reading the daemon's answer is a pure function with a
//! fixture captured from a real daemon, and the transport is the thin part
//! around it. That way the field names are tested without needing Docker
//! running, and only the socket work depends on the machine.

use crate::domain::{ServiceState, ServiceStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};

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

    /// Starts a container that already exists.
    fn start(&self, container: &str) -> Result<(), DockerError>;

    /// Stops a running container, leaving it in place to be started again.
    fn stop(&self, container: &str) -> Result<(), DockerError>;

    /// Restarts a container, which is also how a proxy re-reads a config file
    /// that changed on disk: one request instead of an exec session, and a
    /// local proxy blinking for a moment costs nothing.
    fn restart(&self, container: &str) -> Result<(), DockerError>;
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

/// Pins the name `localhost` to IPv4.
///
/// On Windows the name resolves to ::1 first, and the docker port published
/// from WSL is bound to 127.0.0.1 only, so every request pays a failed
/// connection before falling back. Measured on this machine: 2.45s per command
/// through the name against 0.41s through the address. An address written out
/// in full is left exactly as given.
fn prefer_ipv4(host: &str) -> String {
    match host.split_once(':') {
        Some(("localhost", port)) => format!("127.0.0.1:{port}"),
        None if host == "localhost" => "127.0.0.1".to_string(),
        _ => host.to_string(),
    }
}

#[derive(Deserialize)]
struct ApiStats {
    #[serde(rename = "memory_stats", default)]
    memory: Option<ApiMemory>,
}

#[derive(Deserialize)]
struct ApiMemory {
    usage: Option<u64>,
    #[serde(default)]
    stats: Option<ApiMemoryDetail>,
}

#[derive(Deserialize)]
struct ApiMemoryDetail {
    /// cgroup v2 calls the reclaimable page cache this.
    inactive_file: Option<u64>,
    /// cgroup v1 calls the same thing this.
    cache: Option<u64>,
}

/// What a container is actually using, with the page cache taken out.
///
/// The raw `usage` counts file cache the kernel would drop under pressure, so
/// reporting it as the container's own memory overstates it - on this machine
/// by 226 MB out of 752. `docker stats` subtracts the same figure, so the
/// number here matches what the familiar tool shows.
///
/// Anything unreadable, or a container that is not running, is absent rather
/// than zero: nobody should read "0 MB" as a measurement.
pub fn parse_memory(json: &str) -> Option<u64> {
    let stats: ApiStats = serde_json::from_str(json).ok()?;
    let memory = stats.memory?;
    let usage = memory.usage?;
    let cache = memory
        .stats
        .and_then(|detail| detail.inactive_file.or(detail.cache))
        .unwrap_or(0);
    Some(usage.saturating_sub(cache))
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
        Ok(format!("http://{}", prefer_ipv4(host)))
    } else if let Some(host) = endpoint.strip_prefix("http://") {
        Ok(format!("http://{}", prefer_ipv4(host)))
    } else if endpoint.starts_with("https://") {
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

    fn start(&self, container: &str) -> Result<(), DockerError> {
        self.lifecycle(container, "start")
    }

    fn stop(&self, container: &str) -> Result<(), DockerError> {
        self.lifecycle(container, "stop")
    }

    fn restart(&self, container: &str) -> Result<(), DockerError> {
        self.lifecycle(container, "restart")
    }
}

/// What a container is running and with what, which is where the credentials
/// for a dump come from. Reading them here means this tool never stores a
/// password of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerDetail {
    pub image: String,
    pub env: Vec<String>,
    /// A container started with a terminal writes one unframed stream instead
    /// of the multiplexed pair, so the reader has to know which to expect.
    pub tty: bool,
}

#[derive(Deserialize)]
struct ApiInspect {
    #[serde(rename = "Config")]
    config: ApiInspectConfig,
}

#[derive(Deserialize)]
struct ApiInspectConfig {
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "Env", default)]
    env: Vec<String>,
    #[serde(rename = "Tty", default)]
    tty: bool,
}

pub fn parse_inspect(json: &str) -> Result<ContainerDetail, DockerError> {
    let raw: ApiInspect =
        serde_json::from_str(json).map_err(|error| DockerError::Malformed(error.to_string()))?;
    Ok(ContainerDetail {
        image: raw.config.image,
        env: raw.config.env,
        tty: raw.config.tty,
    })
}

/// How an exec finished. A command that ran and failed is not a transport
/// failure, so it comes back as an outcome rather than an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    pub exit_code: i64,
    pub stderr: Vec<u8>,
}

/// Splits Docker's multiplexed exec stream back into the two streams it
/// carries. Each frame is an eight byte header - stream number, three unused
/// bytes, then a big-endian length - followed by that many bytes.
///
/// stdout is written straight through rather than collected, so a dump larger
/// than memory still works.
pub fn demux_into(source: &mut impl Read, stdout: &mut impl Write) -> std::io::Result<Vec<u8>> {
    const STDERR: u8 = 2;
    let mut stderr = Vec::new();
    let mut header = [0u8; 8];

    loop {
        // A stream that stops between frames is the normal end. A stream that
        // stops inside one is truncated, and either way there is nothing more
        // to read, so both end the loop rather than raising.
        if !read_exactly(source, &mut header)? {
            return Ok(stderr);
        }
        let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if length == 0 {
            continue;
        }

        let mut payload = vec![0u8; length];
        if !read_exactly(source, &mut payload)? {
            return Ok(stderr);
        }
        if header[0] == STDERR {
            stderr.extend_from_slice(&payload);
        } else {
            stdout.write_all(&payload)?;
        }
    }
}

/// Writes both multiplexed streams to one place, in the order they arrived.
///
/// Unlike an exec, where stdout is the result and stderr is commentary, a log
/// is a conversation between the two: separating them loses which line came
/// first, which is usually the thing being looked for.
pub fn demux_merged(source: &mut impl Read, out: &mut impl Write) -> std::io::Result<()> {
    let mut header = [0u8; 8];
    loop {
        if !read_exactly(source, &mut header)? {
            return Ok(());
        }
        let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if length == 0 {
            continue;
        }
        let mut payload = vec![0u8; length];
        if !read_exactly(source, &mut payload)? {
            return Ok(());
        }
        out.write_all(&payload)?;
        // Flushed per frame so following a log shows lines as they land rather
        // than in bursts when a buffer happens to fill.
        out.flush()?;
    }
}

/// Fills `buffer` completely, reporting false when the source ran out first.
fn read_exactly(source: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => return Ok(false),
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

#[derive(Serialize)]
struct ExecRequest<'a> {
    #[serde(rename = "AttachStdout")]
    attach_stdout: bool,
    #[serde(rename = "AttachStderr")]
    attach_stderr: bool,
    #[serde(rename = "Tty")]
    tty: bool,
    #[serde(rename = "Cmd")]
    cmd: &'a [String],
    #[serde(rename = "Env")]
    env: &'a [String],
}

#[derive(Deserialize)]
struct ExecCreated {
    #[serde(rename = "Id")]
    id: String,
}

#[derive(Serialize)]
struct ExecStart {
    #[serde(rename = "Detach")]
    detach: bool,
    #[serde(rename = "Tty")]
    tty: bool,
}

#[derive(Deserialize)]
struct ExecInspect {
    #[serde(rename = "ExitCode")]
    exit_code: Option<i64>,
}

impl HttpDockerClient {
    /// The daemon answers 304 when a container is already in the state asked
    /// for. That is the desired outcome, not a failure, so it is not treated
    /// as one.
    fn lifecycle(&self, container: &str, action: &str) -> Result<(), DockerError> {
        let url = format!("{}/containers/{container}/{action}", self.base);
        match ureq::post(&url).send_empty() {
            Ok(_) => Ok(()),
            Err(ureq::Error::StatusCode(304)) => Ok(()),
            Err(error) => Err(self.unreachable(error.to_string())),
        }
    }

    fn unreachable(&self, reason: String) -> DockerError {
        DockerError::Unreachable {
            endpoint: self.base.clone(),
            reason,
        }
    }

    /// Puts a tar archive into a directory inside a container.
    ///
    /// This is how a file gets in without a two-way connection: a plain HTTP
    /// client cannot write to an exec's stdin, but it can hand the daemon an
    /// archive to unpack.
    pub fn upload(
        &self,
        container: &str,
        directory: &str,
        archive: &[u8],
    ) -> Result<(), DockerError> {
        let url = format!(
            "{}/containers/{container}/archive?path={directory}",
            self.base
        );
        ureq::put(&url)
            .content_type("application/x-tar")
            .send(archive)
            .map_err(|error| self.unreachable(error.to_string()))?;
        Ok(())
    }

    /// Streams a container's log output. `framed` comes from the container's
    /// own Tty setting: with a terminal attached the daemon sends one raw
    /// stream, without one it sends the multiplexed pair.
    pub fn logs(
        &self,
        container: &str,
        follow: bool,
        tail: Option<u32>,
        framed: bool,
        out: &mut impl Write,
    ) -> Result<(), DockerError> {
        let tail = tail.map_or_else(|| "all".to_string(), |lines| lines.to_string());
        let url = format!(
            "{}/containers/{container}/logs?stdout=1&stderr=1&tail={tail}&follow={}",
            self.base,
            u8::from(follow)
        );
        let mut response = ureq::get(&url)
            .call()
            .map_err(|error| self.unreachable(error.to_string()))?;
        let mut reader = response.body_mut().as_reader();

        let outcome = if framed {
            demux_merged(&mut reader, out)
        } else {
            std::io::copy(&mut reader, out).map(|_| ())
        };
        outcome.map_err(|error| self.unreachable(error.to_string()))
    }

    /// Reads one container's memory use.
    ///
    /// Measured on this machine at around 1.5 seconds per container, which is
    /// why nothing calls it unless asked: adding it to every listing would
    /// make the common case pay for the rare one.
    pub fn memory(&self, container: &str) -> Result<Option<u64>, DockerError> {
        let url = format!("{}/containers/{container}/stats?stream=false", self.base);
        let body = ureq::get(&url)
            .call()
            .map_err(|error| self.unreachable(error.to_string()))?
            .body_mut()
            .read_to_string()
            .map_err(|error| self.unreachable(error.to_string()))?;
        Ok(parse_memory(&body))
    }

    pub fn inspect(&self, container: &str) -> Result<ContainerDetail, DockerError> {
        let url = format!("{}/containers/{container}/json", self.base);
        let body = ureq::get(&url)
            .call()
            .map_err(|error| self.unreachable(error.to_string()))?
            .body_mut()
            .read_to_string()
            .map_err(|error| self.unreachable(error.to_string()))?;
        parse_inspect(&body)
    }

    /// Runs a command inside a container and streams its stdout straight to
    /// `stdout`. Nothing is buffered whole, so a dump bigger than memory is
    /// still fine.
    ///
    /// A command that runs and fails is not a transport failure: the exit code
    /// and whatever it said on stderr come back as an outcome, and the caller
    /// decides what that means.
    pub fn exec(
        &self,
        container: &str,
        command: &[String],
        env: &[String],
        stdout: &mut impl Write,
    ) -> Result<ExecOutcome, DockerError> {
        let created: ExecCreated =
            ureq::post(&format!("{}/containers/{container}/exec", self.base))
                .send_json(ExecRequest {
                    attach_stdout: true,
                    attach_stderr: true,
                    // Without a tty the two streams stay separate and framed, which
                    // is what makes a dump readable rather than mixed with warnings.
                    tty: false,
                    cmd: command,
                    env,
                })
                .map_err(|error| self.unreachable(error.to_string()))?
                .body_mut()
                .read_json()
                .map_err(|error| self.unreachable(error.to_string()))?;

        let mut started = ureq::post(&format!("{}/exec/{}/start", self.base, created.id))
            .send_json(ExecStart {
                detach: false,
                tty: false,
            })
            .map_err(|error| self.unreachable(error.to_string()))?;

        let stderr = demux_into(&mut started.body_mut().as_reader(), stdout)
            .map_err(|error| self.unreachable(error.to_string()))?;

        let finished: ExecInspect = ureq::get(&format!("{}/exec/{}/json", self.base, created.id))
            .call()
            .map_err(|error| self.unreachable(error.to_string()))?
            .body_mut()
            .read_json()
            .map_err(|error| self.unreachable(error.to_string()))?;

        Ok(ExecOutcome {
            exit_code: finished.exit_code.unwrap_or(-1),
            stderr,
        })
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
            base_url("http://docker.internal:2375", None).unwrap(),
            "http://docker.internal:2375"
        );
    }

    #[test]
    fn auto_follows_docker_host_the_way_every_other_docker_client_does() {
        assert_eq!(
            base_url("auto", Some("tcp://localhost:2375")).unwrap(),
            "http://127.0.0.1:2375"
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
        // Retried on purpose. Between releasing a port and asking about it, the
        // operating system is free to hand it to something else — and a test
        // that fails once a fortnight in CI teaches people to ignore CI.
        // Answering on all five would still be a real failure.
        let always_answered = (0..5).all(|_| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            probe_port(port, std::time::Duration::from_millis(300))
        });
        assert!(
            !always_answered,
            "a port nothing is listening on must not answer"
        );
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
    /// Trimmed from this machine: only the fields the engine picker needs.
    const REAL_INSPECT: &str = r#"{
      "Id": "abc123",
      "Config": {
        "Image": "mysql:9.7",
        "Env": ["MYSQL_ROOT_PASSWORD=s3cr3t", "PATH=/usr/bin"]
      }
    }"#;

    #[test]
    fn inspecting_a_container_reports_the_image_and_its_environment() {
        let detail = parse_inspect(REAL_INSPECT).unwrap();
        assert_eq!(detail.image, "mysql:9.7");
        assert_eq!(detail.env.len(), 2);
        assert!(detail
            .env
            .contains(&"MYSQL_ROOT_PASSWORD=s3cr3t".to_string()));
    }

    #[test]
    fn a_container_with_no_environment_inspects_to_an_empty_list_not_an_error() {
        let detail = parse_inspect(r#"{"Config":{"Image":"redis:alpine"}}"#).unwrap();
        assert_eq!(detail.image, "redis:alpine");
        assert!(detail.env.is_empty());
    }

    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![stream, 0, 0, 0];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn exec_output_is_split_back_into_the_two_streams_docker_multiplexed() {
        let mut wire = frame(1, b"CREATE TABLE");
        wire.extend(frame(2, b"warning: skipping"));
        wire.extend(frame(1, b" shop;"));

        let mut stdout = Vec::new();
        let stderr = demux_into(&mut std::io::Cursor::new(wire), &mut stdout).unwrap();
        assert_eq!(stdout, b"CREATE TABLE shop;");
        assert_eq!(stderr, b"warning: skipping");
    }

    #[test]
    fn a_stream_cut_off_mid_frame_ends_instead_of_hanging_or_panicking() {
        let mut wire = frame(1, b"partial");
        wire.truncate(wire.len() - 3);
        let mut stdout = Vec::new();
        let stderr = demux_into(&mut std::io::Cursor::new(wire), &mut stdout).unwrap();
        assert!(stderr.is_empty());
        assert!(
            stdout.len() < 7,
            "a truncated frame must not be reported as complete output"
        );
    }

    #[test]
    fn an_empty_stream_produces_nothing_rather_than_failing() {
        let mut stdout = Vec::new();
        let stderr = demux_into(&mut std::io::Cursor::new(Vec::new()), &mut stdout).unwrap();
        assert!(stdout.is_empty() && stderr.is_empty());
    }

    #[test]
    fn localhost_is_pinned_to_ipv4_because_the_ipv6_attempt_costs_seconds() {
        // Measured on this machine: 2.45s through "localhost" against 0.41s
        // through 127.0.0.1, because the name resolves to ::1 first and the
        // published port is only on IPv4.
        assert_eq!(
            base_url("tcp://localhost:2375", None).unwrap(),
            "http://127.0.0.1:2375"
        );
        assert_eq!(
            base_url("auto", Some("tcp://localhost:2375")).unwrap(),
            "http://127.0.0.1:2375"
        );
    }

    #[test]
    fn an_address_written_out_in_full_is_left_alone() {
        assert_eq!(
            base_url("tcp://[::1]:2375", None).unwrap(),
            "http://[::1]:2375"
        );
        assert_eq!(
            base_url("tcp://192.168.1.5:2375", None).unwrap(),
            "http://192.168.1.5:2375"
        );
        assert_eq!(
            base_url("tcp://localhostings:2375", None).unwrap(),
            "http://localhostings:2375",
            "only the name localhost itself is redirected, not anything containing it"
        );
    }

    #[test]
    fn inspecting_reports_whether_the_container_has_a_terminal_attached() {
        let with_tty = parse_inspect(r#"{"Config":{"Image":"x","Tty":true}}"#).unwrap();
        let without = parse_inspect(REAL_INSPECT).unwrap();
        assert!(
            with_tty.tty && !without.tty,
            "a container with a terminal sends its output unframed, so the reader \
             has to know which shape to expect"
        );
    }

    #[test]
    fn merged_output_keeps_the_two_streams_in_the_order_they_arrived() {
        let mut wire = frame(1, b"starting\n");
        wire.extend(frame(2, b"warning: no config\n"));
        wire.extend(frame(1, b"ready\n"));

        let mut out = Vec::new();
        demux_merged(&mut std::io::Cursor::new(wire), &mut out).unwrap();
        assert_eq!(
            out, b"starting\nwarning: no config\nready\n",
            "a log is a conversation between the two streams; separating them loses \
             which line came first"
        );
    }

    /// Shaped like this machine's daemon answers, with the numbers it gave.
    const REAL_STATS: &str = r#"{
      "memory_stats": {
        "usage": 752283648,
        "max_usage": 800000000,
        "limit": 8332890112,
        "stats": { "inactive_file": 226324480, "active_file": 1000 }
      }
    }"#;

    #[test]
    fn memory_excludes_page_cache_the_way_docker_stats_does() {
        assert_eq!(
            parse_memory(REAL_STATS),
            Some(752283648 - 226324480),
            "reporting raw usage counts the page cache as the container's own, \
             which overstates it by hundreds of megabytes"
        );
    }

    #[test]
    fn an_older_cgroup_layout_reports_the_same_thing_under_another_name() {
        let v1 = r#"{"memory_stats":{"usage":1000,"stats":{"cache":400}}}"#;
        assert_eq!(parse_memory(v1), Some(600));
    }

    #[test]
    fn a_stopped_container_reports_no_memory_rather_than_zero() {
        assert_eq!(parse_memory(r#"{"memory_stats":{}}"#), None);
        assert_eq!(parse_memory("{}"), None);
    }

    #[test]
    fn a_cache_larger_than_the_usage_does_not_wrap_around() {
        let odd = r#"{"memory_stats":{"usage":100,"stats":{"inactive_file":999}}}"#;
        assert_eq!(
            parse_memory(odd),
            Some(0),
            "subtracting past zero on unsigned numbers would report gigabytes"
        );
    }

    #[test]
    fn stats_that_cannot_be_read_are_absent_rather_than_an_error() {
        assert_eq!(parse_memory("not json"), None);
    }

    /// A daemon that says exactly one thing and then goes away.
    ///
    /// Every other test here hands a parser a string. These hand the client a
    /// socket, which is the only way to cover what sits between: the URL it
    /// builds, the request it writes, the status it reads, and what it does
    /// with a body that is not what it hoped for. That layer is what would
    /// break against somebody else's docker, and it had no test at all.
    fn fake_daemon(
        status: &str,
        body: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = listener.local_addr().unwrap().port();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        // What the client actually asked for. Nothing else here can see the
        // URL that gets built, and a wrong one against a real daemon comes
        // back as a 404 that points nowhere near this code.
        let (asked, heard) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            // One request, then done: every test here makes exactly one.
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{BufRead, BufReader, Write};
                // The request has to be read before answering, or the client
                // sees the connection close under its own write.
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let mut first = None;
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if first.is_none() {
                        first = Some(line.trim_end().to_string());
                    }
                    if line == "\r\n" {
                        break;
                    }
                    line.clear();
                }
                let _ = asked.send(first.unwrap_or_default());
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        (format!("tcp://127.0.0.1:{port}"), heard)
    }

    #[test]
    fn a_listing_travels_from_a_socket_to_services() {
        let (endpoint, asked) = fake_daemon(
            "200 OK",
            r#"[{"Names":["/mysql-db"],"State":"running","Image":"mysql:8",
                 "Labels":{"com.docker.compose.service":"mysql"},
                 "Ports":[{"PublicPort":3306,"Type":"tcp"}]}]"#,
        );
        let client = HttpDockerClient::new(&endpoint, None).expect("an endpoint");

        let services = client.services().expect("a listing");

        assert_eq!(
            asked.recv().unwrap(),
            "GET /containers/json?all=true HTTP/1.1",
            "the path and the query, which nothing else here can see"
        );
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].service, "mysql");
        assert_eq!(services[0].container, "mysql-db");
        assert_eq!(services[0].port, Some(3306));
        assert_eq!(services[0].state, ServiceState::Running);
    }

    #[test]
    fn a_daemon_that_answers_with_an_error_is_not_read_as_an_empty_listing() {
        let (endpoint, _asked) = fake_daemon("500 Internal Server Error", r#"{"message":"boom"}"#);
        let client = HttpDockerClient::new(&endpoint, None).unwrap();

        let outcome = client.services();
        assert!(
            outcome.is_err(),
            "a daemon in trouble reporting nothing running is the lie this \
             whole project exists to stop telling"
        );
    }

    #[test]
    fn a_body_that_is_not_the_shape_expected_is_an_error_with_the_reason() {
        let (endpoint, _asked) = fake_daemon("200 OK", r#"{"not":"a list"}"#);
        let client = HttpDockerClient::new(&endpoint, None).unwrap();

        let error = client.services().unwrap_err().to_string();
        assert!(
            !error.is_empty(),
            "a docker that answers in a shape this does not know has to say so"
        );
    }

    #[test]
    fn a_socket_that_answers_nothing_at_all_is_reported_as_unreachable() {
        // Bound and dropped: the port is closed, so connecting fails outright.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let client = HttpDockerClient::new(&format!("tcp://127.0.0.1:{port}"), None).unwrap();
        let error = client.services().unwrap_err();
        assert!(
            matches!(error, DockerError::Unreachable { .. }),
            "got {error:?}"
        );
        assert!(
            error.to_string().contains(&port.to_string()),
            "and it names what it could not reach: {error}"
        );
    }
}
