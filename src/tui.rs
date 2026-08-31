//! The dashboard.
//!
//! State and drawing are separated on purpose. Everything on `Dashboard` is
//! plain data with no terminal involved, which is what makes a terminal UI
//! testable at all; the ratatui loop around it only pushes keys in and takes a
//! rendered screen out.
//!
//! The rule the predecessor broke is enforced by shape here: updates arrive as
//! messages from collectors running elsewhere, and drawing never waits on one.

use crate::domain::{Project, ServiceStatus};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Projects,
    Services,
    Ports,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Services => "Services",
            Tab::Ports => "Ports",
        }
    }

    fn next(self) -> Tab {
        match self {
            Tab::Projects => Tab::Services,
            Tab::Services => Tab::Ports,
            Tab::Ports => Tab::Projects,
        }
    }
}

/// Something a collector learned. Collectors never touch the dashboard
/// directly; they send one of these and carry on working.
#[derive(Debug)]
pub enum Update {
    Project(Project),
    ScanFailed { path: PathBuf, reason: String },
    ScanFinished { scanned: usize },
    Services(Vec<ServiceStatus>),
    ServicesFailed(String),
}

pub struct Dashboard {
    projects: Vec<Project>,
    scan_failures: Vec<(PathBuf, String)>,
    scanned: Option<usize>,
    services: Vec<ServiceStatus>,
    services_error: Option<String>,
    tab: Tab,
    selected: usize,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Dashboard {
    pub fn new() -> Self {
        Self {
            projects: Vec::new(),
            scan_failures: Vec::new(),
            scanned: None,
            services: Vec::new(),
            services_error: None,
            tab: Tab::Projects,
            selected: 0,
        }
    }

    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Project(project) => self.projects.push(project),
            Update::ScanFailed { path, reason } => self.scan_failures.push((path, reason)),
            Update::ScanFinished { scanned } => self.scanned = Some(scanned),
            Update::Services(services) => {
                self.services = services;
                self.services_error = None;
            }
            Update::ServicesFailed(reason) => self.services_error = Some(reason),
        }
    }

    /// Clears what a refresh is about to replace, so a second run cannot stack
    /// its results on top of the first.
    pub fn begin_refresh(&mut self) {
        self.projects.clear();
        self.scan_failures.clear();
        self.scanned = None;
        self.selected = 0;
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn is_scanning(&self) -> bool {
        self.scanned.is_none()
    }

    pub fn next_tab(&mut self) {
        self.tab = self.tab.next();
        // A row number taken from a longer list would point past the end of a
        // shorter one, so it does not survive the move.
        self.selected = 0;
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.selected = 0;
    }

    pub fn move_selection(&mut self, delta: isize) {
        let last = self.row_count().saturating_sub(1) as isize;
        let target = self.selected as isize + delta;
        self.selected = target.clamp(0, last) as usize;
    }

    pub fn row_count(&self) -> usize {
        match self.tab {
            Tab::Projects => self.projects.len(),
            Tab::Services => self.services.len(),
            Tab::Ports => self.services.iter().filter(|s| s.port.is_some()).count(),
        }
    }

    fn rows(&self) -> Vec<String> {
        match self.tab {
            Tab::Projects => self
                .projects
                .iter()
                .map(|project| {
                    format!(
                        "{:<28} {:<12} {:<9} {:<22} {}",
                        clip(&project.name, 28),
                        clip(project.category.as_deref().unwrap_or("-"), 12),
                        format!("{:?}", project.stack),
                        clip(project.git.branch.as_deref().unwrap_or("-"), 22),
                        project.git.badge()
                    )
                })
                .collect(),
            Tab::Services => self
                .services
                .iter()
                .map(|service| {
                    format!(
                        "{:<18} {:<24} {:<7} {}",
                        clip(&service.service, 18),
                        clip(&service.container, 24),
                        service
                            .port
                            .map_or_else(|| "-".to_string(), |port| port.to_string()),
                        service.condition()
                    )
                })
                .collect(),
            Tab::Ports => self
                .services
                .iter()
                .filter(|service| service.port.is_some())
                .map(|service| {
                    format!(
                        "{:>6}  {:<18} {}",
                        service.port.unwrap_or_default(),
                        clip(&service.service, 18),
                        if service.port_open {
                            "answering"
                        } else {
                            "no answer"
                        }
                    )
                })
                .collect(),
        }
    }

    fn header(&self) -> String {
        let tabs: Vec<String> = [Tab::Projects, Tab::Services, Tab::Ports]
            .iter()
            .map(|tab| {
                if *tab == self.tab {
                    format!("[{}]", tab.label())
                } else {
                    format!(" {} ", tab.label())
                }
            })
            .collect();
        format!(
            "aether-dev {}  tab switch  j/k move  r refresh  q quit",
            tabs.join(" ")
        )
    }

    fn summary(&self) -> String {
        match self.tab {
            Tab::Projects => {
                let examined = match self.scanned {
                    // The denominator is part of the answer: "no projects" and
                    // "nothing was examined" are different facts.
                    Some(count) => format!("{count} directories examined"),
                    None => "scanning...".to_string(),
                };
                format!(
                    "{} projects, {} unreadable, {examined}",
                    self.projects.len(),
                    self.scan_failures.len()
                )
            }
            Tab::Services | Tab::Ports => match &self.services_error {
                // An unreachable daemon must not read as a daemon with nothing
                // running, so the count is replaced rather than shown as zero.
                Some(reason) => format!("docker unreachable: {reason}"),
                None if self.tab == Tab::Services => {
                    let ready = self.services.iter().filter(|s| s.is_reachable()).count();
                    format!("{} services, {ready} ready", self.services.len())
                }
                None => {
                    let published: Vec<&ServiceStatus> =
                        self.services.iter().filter(|s| s.port.is_some()).collect();
                    let answering = published.iter().filter(|s| s.port_open).count();
                    format!("{} published ports, {answering} answering", published.len())
                }
            },
        }
    }

    /// Draws the whole screen as text. Returning a string rather than painting
    /// widgets directly is what lets the tests assert on what a user sees.
    pub fn render(&self, width: u16, height: u16) -> String {
        let width = (width as usize).max(20);
        let height = (height as usize).max(6);
        let capacity = height - 4;

        let mut lines = Vec::with_capacity(height);
        lines.push(clip(&self.header(), width));
        lines.push("-".repeat(width));

        let rows = self.rows();
        // Scroll only as far as needed to keep the current row on screen.
        let offset = self.selected.saturating_sub(capacity.saturating_sub(1));
        for (index, row) in rows.iter().enumerate().skip(offset).take(capacity) {
            let marker = if index == self.selected { '>' } else { ' ' };
            lines.push(format!("{marker}{}", clip(row, width - 1)));
        }
        while lines.len() < height - 2 {
            lines.push(String::new());
        }

        lines.push("-".repeat(width));
        lines.push(clip(&self.summary(), width));
        lines.truncate(height);
        lines.join("\n")
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{GitStatus, ServiceState, ServiceStatus, Stack};
    use std::path::PathBuf;

    fn project(name: &str) -> Project {
        Project {
            name: name.to_string(),
            category: Some("group".to_string()),
            path: PathBuf::from(name),
            stack: Stack::Rust,
            git: GitStatus::clean("main"),
        }
    }

    fn service(name: &str, port: u16, open: bool) -> ServiceStatus {
        ServiceStatus {
            container: format!("{name}-1"),
            service: name.to_string(),
            port: Some(port),
            state: ServiceState::Running,
            port_open: open,
            memory_bytes: None,
        }
    }

    fn with_projects(count: usize) -> Dashboard {
        let mut dashboard = Dashboard::new();
        for index in 0..count {
            dashboard.apply(Update::Project(project(&format!("p{index}"))));
        }
        dashboard.apply(Update::ScanFinished { scanned: count });
        dashboard
    }

    #[test]
    fn projects_accumulate_as_they_arrive_rather_than_all_at_once() {
        let mut dashboard = Dashboard::new();
        assert_eq!(dashboard.row_count(), 0);
        dashboard.apply(Update::Project(project("alpha")));
        assert_eq!(dashboard.row_count(), 1);
        dashboard.apply(Update::Project(project("beta")));
        assert_eq!(dashboard.row_count(), 2);
    }

    #[test]
    fn a_scan_is_marked_loading_until_it_reports_its_total() {
        let mut dashboard = Dashboard::new();
        assert!(dashboard.is_scanning());
        dashboard.apply(Update::Project(project("alpha")));
        assert!(
            dashboard.is_scanning(),
            "one result is not the end of the scan"
        );
        dashboard.apply(Update::ScanFinished { scanned: 4 });
        assert!(!dashboard.is_scanning());
    }

    #[test]
    fn the_selection_cannot_leave_the_list() {
        let mut dashboard = with_projects(3);
        dashboard.move_selection(-1);
        assert_eq!(dashboard.selected(), 0, "moving up from the top must stay");
        dashboard.move_selection(10);
        assert_eq!(
            dashboard.selected(),
            2,
            "moving past the end must stop at the last row"
        );
    }

    #[test]
    fn an_empty_list_keeps_the_selection_at_zero_instead_of_underflowing() {
        let mut dashboard = Dashboard::new();
        dashboard.move_selection(1);
        dashboard.move_selection(-1);
        assert_eq!(dashboard.selected(), 0);
    }

    #[test]
    fn switching_tabs_resets_the_selection_so_it_cannot_point_past_a_shorter_list() {
        let mut dashboard = with_projects(5);
        dashboard.move_selection(4);
        assert_eq!(dashboard.selected(), 4);
        dashboard.next_tab();
        assert_eq!(dashboard.tab(), Tab::Services);
        assert_eq!(dashboard.selected(), 0);
    }

    #[test]
    fn the_tabs_cycle_back_around() {
        let mut dashboard = Dashboard::new();
        assert_eq!(dashboard.tab(), Tab::Projects);
        dashboard.next_tab();
        dashboard.next_tab();
        assert_eq!(dashboard.tab(), Tab::Ports);
        dashboard.next_tab();
        assert_eq!(dashboard.tab(), Tab::Projects);
    }

    #[test]
    fn ports_shows_only_services_that_publish_one() {
        let mut dashboard = Dashboard::new();
        let mut quiet = service("internal", 0, false);
        quiet.port = None;
        dashboard.apply(Update::Services(vec![service("mysql", 3306, true), quiet]));
        dashboard.next_tab();
        assert_eq!(dashboard.row_count(), 2, "services lists everything");
        dashboard.next_tab();
        assert_eq!(
            dashboard.row_count(),
            1,
            "ports lists only what is published"
        );
    }

    #[test]
    fn a_docker_failure_is_shown_as_a_failure_not_as_zero_services() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ServicesFailed("connection refused".to_string()));
        dashboard.next_tab();
        let view = dashboard.render(90, 24);
        assert!(
            view.contains("connection refused"),
            "the reason must reach the screen"
        );
        assert!(
            !view.contains("0 services"),
            "an unreachable daemon must not read as a daemon with nothing running"
        );
    }

    #[test]
    fn the_view_reports_the_denominator_not_only_the_findings() {
        let dashboard = with_projects(0);
        let view = dashboard.render(90, 24);
        assert!(
            view.contains('0') && view.contains("examined"),
            "'no projects' and 'nothing was examined' must not read the same"
        );
    }

    #[test]
    fn the_selected_row_is_marked_in_the_view() {
        let mut dashboard = with_projects(3);
        dashboard.move_selection(1);
        let marked: Vec<&str> = dashboard
            .render(90, 24)
            .lines()
            .filter(|line| line.starts_with('>'))
            .map(|_| "marked")
            .collect();
        assert_eq!(marked.len(), 1, "exactly one row is the current one");
    }

    #[test]
    fn the_view_never_draws_more_rows_than_the_terminal_has_lines() {
        let dashboard = with_projects(200);
        let view = dashboard.render(90, 20);
        assert!(
            view.lines().count() <= 20,
            "drawing past the last line pushes the header off the screen"
        );
    }
}
