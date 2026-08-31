//! The dashboard.
//!
//! State and drawing are separated on purpose. Everything the dashboard knows
//! is plain data; the ratatui layer below only turns it into cells and pushes
//! keys back in.
//!
//! The rule the predecessor broke is enforced by shape: updates arrive as
//! messages from collectors running elsewhere, and drawing never waits on one.

use crate::domain::{Project, ServiceState, ServiceStatus};
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
    TableState, Tabs,
};
use ratatui::Frame;
use std::path::PathBuf;

/// One palette, named by meaning rather than by colour, so a row and the
/// summary line below it cannot disagree about what "ready" looks like.
mod paint {
    use ratatui::style::Color;

    pub const FRAME: Color = Color::DarkGray;
    pub const HEADING: Color = Color::Cyan;
    pub const MUTED: Color = Color::DarkGray;
    pub const GOOD: Color = Color::Green;
    pub const WAITING: Color = Color::Yellow;
    pub const BAD: Color = Color::Red;
    pub const CHANGED: Color = Color::Yellow;
    pub const UNTRACKED: Color = Color::Blue;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Projects,
    Services,
    Ports,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Projects, Tab::Services, Tab::Ports];

    fn label(self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Services => "Services",
            Tab::Ports => "Ports",
        }
    }

    fn index(self) -> usize {
        match self {
            Tab::Projects => 0,
            Tab::Services => 1,
            Tab::Ports => 2,
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
    /// Held rather than rebuilt each frame so the list keeps its scroll
    /// position instead of jumping back whenever the selection moves.
    table: TableState,
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
            table: TableState::default(),
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
        self.set_tab(self.tab.next());
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        // A row number taken from a longer list would point past the end of a
        // shorter one, so it does not survive the move.
        self.selected = 0;
        self.table = TableState::default();
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
            Tab::Ports => self.published().count(),
        }
    }

    fn published(&self) -> impl Iterator<Item = &ServiceStatus> {
        self.services.iter().filter(|s| s.port.is_some())
    }

    /// Draws the whole screen: a tab strip, the list, and one line saying what
    /// the numbers add up to.
    pub fn draw(&mut self, frame: &mut Frame) {
        let [top, middle, bottom] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let [status, keys] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(46)]).areas(bottom);

        frame.render_widget(self.tab_strip(), top);
        self.draw_list(frame, middle);
        frame.render_widget(self.status_line(), status);
        frame.render_widget(key_hints(), keys);
    }

    fn tab_strip(&self) -> Tabs<'_> {
        Tabs::new(Tab::ALL.map(Tab::label).to_vec())
            .select(self.tab.index())
            .style(Style::default().fg(paint::MUTED))
            .highlight_style(
                Style::default()
                    .fg(paint::HEADING)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(Span::styled("·", Style::default().fg(paint::FRAME)))
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        self.table.select(Some(self.selected));

        // Columns are per tab rather than padded to a common shape: a fixed
        // arity meant carrying empty columns, and empty columns took the width
        // that the branch names needed.
        let (header, widths, rows): (Vec<&str>, Vec<Constraint>, Vec<Row<'static>>) = match self.tab
        {
            Tab::Projects => (
                vec!["PROJECT", "GROUP", "FRAMEWORK", "BRANCH", "CHANGES"],
                vec![
                    Constraint::Fill(3),
                    Constraint::Length(12),
                    Constraint::Length(17),
                    Constraint::Fill(3),
                    Constraint::Length(10),
                ],
                self.project_rows(),
            ),
            Tab::Services => (
                vec!["SERVICE", "CONTAINER", "PORT", "STATE"],
                vec![
                    Constraint::Length(18),
                    Constraint::Fill(1),
                    Constraint::Length(7),
                    Constraint::Length(9),
                ],
                self.service_rows(),
            ),
            Tab::Ports => (
                vec!["PORT", "SERVICE", "ANSWERS"],
                vec![
                    Constraint::Length(7),
                    Constraint::Length(26),
                    Constraint::Fill(1),
                ],
                self.port_rows(),
            ),
        };

        let table = Table::new(rows, widths)
            .header(
                Row::new(header).style(
                    Style::default()
                        .fg(paint::MUTED)
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .block(
                Block::bordered()
                    .border_style(Style::default().fg(paint::FRAME))
                    .padding(Padding::horizontal(1))
                    .title(Span::styled(
                        format!(" {} ", self.tab.label()),
                        Style::default()
                            .fg(paint::HEADING)
                            .add_modifier(Modifier::BOLD),
                    )),
            )
            .column_spacing(2)
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        frame.render_stateful_widget(table, area, &mut self.table);

        // Only when there is more than fits: a scrollbar on a list that is
        // entirely visible tells the reader nothing and takes a column doing it.
        let rows = self.row_count();
        let visible = usize::from(area.height.saturating_sub(3));
        if rows > visible && visible > 0 {
            let mut position = ScrollbarState::new(rows).position(self.selected);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    // Matching the border so the bar reads as part of the frame
                    // rather than as a second one drawn on top of it.
                    .track_symbol(Some("│"))
                    .track_style(Style::default().fg(paint::FRAME))
                    .thumb_style(Style::default().fg(paint::HEADING)),
                area.inner(Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut position,
            );
        }
    }

    fn project_rows(&self) -> Vec<Row<'static>> {
        self.projects
            .iter()
            .map(|project| {
                let framework = project
                    .framework
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", project.stack));
                Row::new(vec![
                    Cell::from(project.name.clone()),
                    Cell::from(project.category.clone().unwrap_or_else(|| "-".into()))
                        .style(Style::default().fg(paint::MUTED)),
                    Cell::from(framework).style(Style::default().fg(paint::HEADING)),
                    Cell::from(project.git.branch.clone().unwrap_or_else(|| "-".into())),
                    Cell::from(changes(&project.git)),
                ])
            })
            .collect()
    }

    fn service_rows(&self) -> Vec<Row<'static>> {
        self.services
            .iter()
            .map(|service| {
                Row::new(vec![
                    Cell::from(service.service.clone()),
                    Cell::from(service.container.clone()).style(Style::default().fg(paint::MUTED)),
                    Cell::from(
                        service
                            .port
                            .map_or_else(|| "-".to_string(), |port| port.to_string()),
                    ),
                    Cell::from(service.condition()).style(condition_style(service)),
                    Cell::from(""),
                ])
            })
            .collect()
    }

    fn port_rows(&self) -> Vec<Row<'static>> {
        self.published()
            .map(|service| {
                let (answer, style) = if service.port_open {
                    ("answering", Style::default().fg(paint::GOOD))
                } else {
                    ("no answer", Style::default().fg(paint::BAD))
                };
                Row::new(vec![
                    Cell::from(service.port.unwrap_or_default().to_string()),
                    Cell::from(service.service.clone()),
                    Cell::from(answer).style(style),
                    Cell::from(""),
                    Cell::from(""),
                ])
            })
            .collect()
    }

    fn status_line(&self) -> Paragraph<'_> {
        let dim = Style::default().fg(paint::MUTED);
        let spans = match self.tab {
            Tab::Projects => {
                let examined = match self.scanned {
                    // The denominator is part of the answer: "no projects" and
                    // "nothing was examined" are different facts.
                    Some(count) => format!("{count} directories examined"),
                    None => "scanning…".to_string(),
                };
                let mut spans = vec![
                    Span::styled(
                        format!(" {} projects", self.projects.len()),
                        Style::default().fg(paint::GOOD),
                    ),
                    Span::styled(format!(" · {examined}"), dim),
                ];
                if !self.scan_failures.is_empty() {
                    spans.push(Span::styled(
                        format!(" · {} unreadable", self.scan_failures.len()),
                        Style::default().fg(paint::BAD),
                    ));
                }
                spans
            }
            Tab::Services | Tab::Ports => match &self.services_error {
                // An unreachable daemon must not read as a daemon with nothing
                // running, so the count is replaced rather than shown as zero.
                Some(reason) => vec![Span::styled(
                    format!(" docker unreachable: {reason}"),
                    Style::default().fg(paint::BAD),
                )],
                None if self.tab == Tab::Services => {
                    let ready = self.services.iter().filter(|s| s.is_reachable()).count();
                    vec![
                        Span::styled(format!(" {ready} ready"), Style::default().fg(paint::GOOD)),
                        Span::styled(format!(" · {} services", self.services.len()), dim),
                    ]
                }
                None => {
                    let published: Vec<&ServiceStatus> = self.published().collect();
                    let answering = published.iter().filter(|s| s.port_open).count();
                    vec![
                        Span::styled(
                            format!(" {answering} answering"),
                            Style::default().fg(paint::GOOD),
                        ),
                        Span::styled(format!(" · {} published", published.len()), dim),
                    ]
                }
            },
        };

        Paragraph::new(Line::from(spans))
    }
}

/// Tracked changes and untracked files, coloured apart because they mean
/// different things: one is work in progress, the other is work not yet known
/// to git at all.
fn changes(git: &crate::domain::GitStatus) -> Line<'static> {
    let mut spans = Vec::new();
    if git.modified > 0 {
        spans.push(Span::styled(
            format!("*{}", git.modified),
            Style::default().fg(paint::CHANGED),
        ));
    }
    if git.untracked > 0 {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("?{}", git.untracked),
            Style::default().fg(paint::UNTRACKED),
        ));
    }
    Line::from(spans)
}

fn condition_style(service: &ServiceStatus) -> Style {
    match (service.state, service.port_open) {
        (ServiceState::Running, true) => Style::default()
            .fg(paint::GOOD)
            .add_modifier(Modifier::BOLD),
        (ServiceState::Running, false) => Style::default().fg(paint::WAITING),
        (ServiceState::Stopped, _) => Style::default().fg(paint::MUTED),
    }
}

/// The keys, kept to the right so the numbers on the left are read first.
fn key_hints() -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        "tab/1-3 switch · j/k move · r refresh · q quit ",
        Style::default().fg(paint::FRAME),
    )))
    .alignment(Alignment::Right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{GitStatus, Stack};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn project(name: &str) -> Project {
        Project {
            name: name.to_string(),
            category: Some("group".to_string()),
            path: PathBuf::from(name),
            stack: Stack::Rust,
            framework: None,
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

    /// Draws onto a terminal that only exists in memory, so the tests can
    /// assert on what a user would actually see rather than on internals.
    fn drawn(dashboard: &mut Dashboard, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| dashboard.draw(frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn lines(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn screen(buffer: &Buffer) -> String {
        lines(buffer).join("\n")
    }

    /// The style of the row whose text contains `needle`, taken from a cell
    /// well inside it so the border is not what gets measured.
    fn row_style(buffer: &Buffer, needle: &str) -> Option<Style> {
        lines(buffer)
            .iter()
            .position(|line| line.contains(needle))
            .map(|y| buffer[(2, y as u16)].style())
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
        let view = screen(&drawn(&mut dashboard, 90, 12));
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
        let mut dashboard = with_projects(0);
        let view = screen(&drawn(&mut dashboard, 90, 12));
        assert!(
            view.contains("0 projects") && view.contains("examined"),
            "'no projects' and 'nothing was examined' must not read the same"
        );
    }

    #[test]
    fn the_current_row_is_drawn_differently_from_the_others() {
        let mut dashboard = with_projects(3);
        dashboard.move_selection(1);
        let buffer = drawn(&mut dashboard, 90, 12);

        let current = row_style(&buffer, "p1").expect("the selected row is on screen");
        let other = row_style(&buffer, "p2").expect("another row is on screen");
        assert_ne!(
            current, other,
            "a list where the current row looks like every other row cannot be navigated"
        );
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_current_row_on_screen() {
        let mut dashboard = with_projects(200);
        dashboard.move_selection(150);
        let view = screen(&drawn(&mut dashboard, 90, 12));
        assert!(
            view.contains("p150"),
            "a selection the list has scrolled past is a selection nobody can see"
        );
    }

    #[test]
    fn a_service_that_is_ready_is_not_drawn_like_one_that_is_stopped() {
        let mut dashboard = Dashboard::new();
        let mut stopped = service("redis", 6379, false);
        stopped.state = ServiceState::Stopped;
        dashboard.apply(Update::Services(vec![
            service("mysql", 3306, true),
            stopped,
        ]));
        dashboard.next_tab();
        let buffer = drawn(&mut dashboard, 90, 12);
        let view = screen(&buffer);

        assert!(view.contains("ready") && view.contains("stopped"));
        // Colour carries the same distinction the word does, so the state can
        // be read at a glance rather than word by word.
        let ready_row = lines(&buffer)
            .iter()
            .position(|line| line.contains("mysql"))
            .expect("mysql row");
        let stopped_row = lines(&buffer)
            .iter()
            .position(|line| line.contains("redis"))
            .expect("redis row");
        let rows = lines(&buffer);
        let ready_at = rows[ready_row].find("ready").expect("the word ready") as u16;
        let stopped_at = rows[stopped_row].find("stopped").expect("the word stopped") as u16;
        assert_ne!(
            buffer[(ready_at, ready_row as u16)].style().fg,
            buffer[(stopped_at, stopped_row as u16)].style().fg
        );
    }

    #[test]
    fn the_tab_you_are_on_is_marked_in_the_strip() {
        let mut dashboard = Dashboard::new();
        let buffer = drawn(&mut dashboard, 90, 12);
        let strip = lines(&buffer)[0].clone();
        assert!(strip.contains("Projects") && strip.contains("Services"));

        let projects_at = strip.find("Projects").expect("Projects in the strip") as u16;
        let services_at = strip.find("Services").expect("Services in the strip") as u16;
        assert_ne!(
            buffer[(projects_at, 0)].style(),
            buffer[(services_at, 0)].style(),
            "if every tab looks the same, the strip does not say where you are"
        );
    }

    #[test]
    fn the_frame_carries_the_name_of_the_list_being_shown() {
        let mut dashboard = with_projects(1);
        assert!(screen(&drawn(&mut dashboard, 90, 12)).contains("Projects"));
        dashboard.next_tab();
        assert!(screen(&drawn(&mut dashboard, 90, 12)).contains("Services"));
    }

    #[test]
    fn a_list_that_fits_gets_no_scrollbar_and_a_longer_one_does() {
        let mut fits = with_projects(3);
        let mut overflows = with_projects(200);
        assert!(
            !screen(&drawn(&mut fits, 90, 12)).contains('█'),
            "a scrollbar on a list you can see all of takes a column and says nothing"
        );
        assert!(
            screen(&drawn(&mut overflows, 90, 12)).contains('█'),
            "without it there is no way to tell where in 200 rows you are"
        );
    }
}
