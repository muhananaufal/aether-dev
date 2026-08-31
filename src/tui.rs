//! The dashboard.
//!
//! Three panes at once rather than three tabs. Tabs hide two thirds of what
//! the tool knows at any moment, and the three lists fit side by side on any
//! terminal wide enough to be worth using — so the question "is the database
//! up" does not need a keystroke to answer.
//!
//! State and drawing are separated: everything the dashboard knows is plain
//! data, and the ratatui layer only turns it into cells and pushes keys back
//! in. Updates arrive as messages from collectors running elsewhere, and
//! drawing never waits on one — the rule the predecessor broke.

use crate::domain::{GitStatus, Project, ServiceState, ServiceStatus};
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
    TableState,
};
use ratatui::Frame;
use std::path::PathBuf;

/// Below this the three panes would each be too narrow to read, so the focused
/// one takes the whole screen instead. A cramped layout is worse than one list.
const SIDE_BY_SIDE_WIDTH: u16 = 100;

/// One palette, named by meaning rather than by colour, so a row and the
/// summary line below it cannot disagree about what "ready" looks like.
mod paint {
    use ratatui::style::Color;

    pub const FRAME: Color = Color::DarkGray;
    pub const FOCUS: Color = Color::Cyan;
    pub const HEADING: Color = Color::Cyan;
    pub const MUTED: Color = Color::DarkGray;
    pub const GOOD: Color = Color::Green;
    pub const WAITING: Color = Color::Yellow;
    pub const BAD: Color = Color::Red;
    pub const CHANGED: Color = Color::Yellow;
    pub const UNTRACKED: Color = Color::Blue;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Projects,
    Services,
    Ports,
}

impl Pane {
    pub const ALL: [Pane; 3] = [Pane::Projects, Pane::Services, Pane::Ports];

    fn title(self) -> &'static str {
        match self {
            Pane::Projects => "Projects",
            Pane::Services => "Services",
            Pane::Ports => "Ports",
        }
    }

    fn index(self) -> usize {
        match self {
            Pane::Projects => 0,
            Pane::Services => 1,
            Pane::Ports => 2,
        }
    }

    fn next(self) -> Pane {
        match self {
            Pane::Projects => Pane::Services,
            Pane::Services => Pane::Ports,
            Pane::Ports => Pane::Projects,
        }
    }

    fn previous(self) -> Pane {
        match self {
            Pane::Projects => Pane::Ports,
            Pane::Services => Pane::Projects,
            Pane::Ports => Pane::Services,
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
    focus: Pane,
    /// One selection per pane. With all three on screen at once, a shared
    /// cursor would move a list nobody was looking at.
    selected: [usize; 3],
    /// Held rather than rebuilt each frame so a list keeps its scroll position
    /// instead of jumping back whenever the selection moves.
    tables: [TableState; 3],
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
            focus: Pane::Projects,
            selected: [0; 3],
            tables: Default::default(),
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
                self.clamp(Pane::Services);
                self.clamp(Pane::Ports);
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
        self.selected[Pane::Projects.index()] = 0;
    }

    pub fn focus(&self) -> Pane {
        self.focus
    }

    pub fn selected(&self) -> usize {
        self.selected[self.focus.index()]
    }

    pub fn selected_in(&self, pane: Pane) -> usize {
        self.selected[pane.index()]
    }

    pub fn is_scanning(&self) -> bool {
        self.scanned.is_none()
    }

    /// Moves the focus. Each pane keeps the row it was on, because coming back
    /// to a list and finding it reset is how a dashboard loses your place.
    pub fn focus_next(&mut self) {
        self.focus = self.focus.next();
    }

    pub fn focus_previous(&mut self) {
        self.focus = self.focus.previous();
    }

    pub fn focus_on(&mut self, pane: Pane) {
        self.focus = pane;
    }

    pub fn move_selection(&mut self, delta: isize) {
        let pane = self.focus;
        let last = self.rows_in(pane).saturating_sub(1) as isize;
        let target = self.selected[pane.index()] as isize + delta;
        self.selected[pane.index()] = target.clamp(0, last) as usize;
    }

    /// Keeps a selection inside a list that just got shorter.
    fn clamp(&mut self, pane: Pane) {
        let last = self.rows_in(pane).saturating_sub(1);
        let current = &mut self.selected[pane.index()];
        *current = (*current).min(last);
    }

    pub fn rows_in(&self, pane: Pane) -> usize {
        match pane {
            Pane::Projects => self.projects.len(),
            Pane::Services => self.services.len(),
            Pane::Ports => self.published().count(),
        }
    }

    fn published(&self) -> impl Iterator<Item = &ServiceStatus> {
        self.services.iter().filter(|s| s.port.is_some())
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let [body, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

        if body.width < SIDE_BY_SIDE_WIDTH {
            // Too narrow to divide: show what has the focus, whole.
            self.draw_pane(frame, self.focus, body);
        } else {
            let [left, right] =
                Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)]).areas(body);
            let [top, bottom] =
                Layout::vertical([Constraint::Fill(1), Constraint::Fill(1)]).areas(right);

            self.draw_pane(frame, Pane::Projects, left);
            self.draw_pane(frame, Pane::Services, top);
            self.draw_pane(frame, Pane::Ports, bottom);
        }

        // On a narrow terminal the hints would run into the counts and neither
        // would be readable. The numbers are what the line is for, so the
        // hints are the half that goes.
        if status.width >= 90 {
            let [summary, keys] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(44)]).areas(status);
            frame.render_widget(self.status_line(), summary);
            frame.render_widget(key_hints(), keys);
        } else {
            frame.render_widget(self.status_line(), status);
        }
    }

    fn draw_pane(&mut self, frame: &mut Frame, pane: Pane, area: Rect) {
        let focused = pane == self.focus;
        let index = pane.index();
        self.tables[index].select(Some(self.selected[index]));

        let (header, widths, rows): (Vec<&str>, Vec<Constraint>, Vec<Row<'static>>) = match pane {
            Pane::Projects => (
                vec!["PROJECT", "FRAMEWORK", "BRANCH", ""],
                vec![
                    Constraint::Fill(3),
                    Constraint::Length(17),
                    Constraint::Fill(3),
                    Constraint::Length(9),
                ],
                self.project_rows(),
            ),
            Pane::Services => (
                vec!["SERVICE", "PORT", "STATE"],
                vec![
                    Constraint::Fill(1),
                    Constraint::Length(7),
                    Constraint::Length(9),
                ],
                self.service_rows(),
            ),
            Pane::Ports => (
                vec!["PORT", "SERVICE", ""],
                vec![
                    Constraint::Length(7),
                    Constraint::Fill(1),
                    Constraint::Length(10),
                ],
                self.port_rows(),
            ),
        };

        // The focused pane is the one the keys reach, so its frame and title
        // say so; the others stay quiet rather than competing for attention.
        let border = if focused { paint::FOCUS } else { paint::FRAME };
        let title = if focused {
            Style::default()
                .fg(paint::FOCUS)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(paint::MUTED)
        };
        let highlight = if focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            // Still visible, so returning to a pane shows where you left off,
            // but not shouting from a list the keys are not reaching.
            Style::default().fg(paint::FOCUS)
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
                    .border_style(Style::default().fg(border))
                    .padding(Padding::horizontal(1))
                    .title(Span::styled(
                        format!(" {} {} ", pane.index() + 1, pane.title()),
                        title,
                    ))
                    .title_bottom(Span::styled(
                        format!(" {} ", self.pane_count(pane)),
                        Style::default().fg(paint::MUTED),
                    )),
            )
            .column_spacing(2)
            .row_highlight_style(highlight);

        frame.render_stateful_widget(table, area, &mut self.tables[index]);

        // Only when there is more than fits: a scrollbar on a list you can see
        // all of takes a column and says nothing.
        let rows = self.rows_in(pane);
        let visible = usize::from(area.height.saturating_sub(3));
        if rows > visible && visible > 0 {
            let mut position = ScrollbarState::new(rows).position(self.selected[index]);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    // Matching the border so the bar reads as part of the frame
                    // rather than a second one drawn on top of it.
                    .track_symbol(Some("│"))
                    .track_style(Style::default().fg(border))
                    .thumb_style(Style::default().fg(paint::FOCUS)),
                area.inner(Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut position,
            );
        }
    }

    /// The count on each pane's frame, so a glance says how much is in a list
    /// that is scrolled or empty.
    fn pane_count(&self, pane: Pane) -> String {
        match pane {
            Pane::Projects => match self.scanned {
                Some(examined) => format!("{} of {examined} examined", self.projects.len()),
                None => format!("{} so far…", self.projects.len()),
            },
            // A count of zero out of zero would read as a daemon with nothing
            // running, which is a different thing from a daemon nobody could
            // reach. Neither pane pretends to a number it does not have.
            Pane::Services if self.services_error.is_some() => "unreachable".to_string(),
            Pane::Ports if self.services_error.is_some() => "unreachable".to_string(),
            Pane::Services => {
                let ready = self.services.iter().filter(|s| s.is_reachable()).count();
                format!("{ready} of {} ready", self.services.len())
            }
            Pane::Ports => {
                let published: Vec<&ServiceStatus> = self.published().collect();
                let answering = published.iter().filter(|s| s.port_open).count();
                format!("{answering} of {} answering", published.len())
            }
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
                    Cell::from(framework).style(Style::default().fg(paint::HEADING)),
                    Cell::from(project.git.branch.clone().unwrap_or_else(|| "-".into()))
                        .style(Style::default().fg(paint::MUTED)),
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
                    Cell::from(
                        service
                            .port
                            .map_or_else(|| "-".to_string(), |port| port.to_string()),
                    )
                    .style(Style::default().fg(paint::MUTED)),
                    Cell::from(service.condition()).style(condition_style(service)),
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
                ])
            })
            .collect()
    }

    fn status_line(&self) -> Paragraph<'static> {
        let dim = Style::default().fg(paint::MUTED);
        let mut spans = vec![Span::styled(
            format!(" {} projects", self.projects.len()),
            Style::default().fg(paint::GOOD),
        )];

        match self.scanned {
            // The denominator is part of the answer: "no projects" and
            // "nothing was examined" are different facts.
            Some(count) => spans.push(Span::styled(format!(" of {count} directories"), dim)),
            None => spans.push(Span::styled(" · scanning…".to_string(), dim)),
        }
        if !self.scan_failures.is_empty() {
            spans.push(Span::styled(
                format!(" · {} unreadable", self.scan_failures.len()),
                Style::default().fg(paint::BAD),
            ));
        }

        match &self.services_error {
            // An unreachable daemon must not read as a daemon with nothing
            // running, so the counts are replaced rather than shown as zero.
            Some(reason) => spans.push(Span::styled(
                format!("   docker unreachable: {reason}"),
                Style::default().fg(paint::BAD),
            )),
            None => {
                let ready = self.services.iter().filter(|s| s.is_reachable()).count();
                spans.push(Span::styled(
                    format!("   {ready} ready"),
                    Style::default().fg(paint::GOOD),
                ));
                spans.push(Span::styled(
                    format!(" of {} services", self.services.len()),
                    dim,
                ));
            }
        }

        Paragraph::new(Line::from(spans))
    }
}

/// The keys, kept to the right so the numbers on the left are read first.
fn key_hints() -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        "tab/1-3 pane · j/k move · r refresh · q quit ",
        Style::default().fg(paint::FRAME),
    )))
    .alignment(Alignment::Right)
}

/// Tracked changes and untracked files, coloured apart because they mean
/// different things: one is work in progress, the other is work git has not
/// been told about at all.
fn changes(git: &GitStatus) -> Line<'static> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Stack;
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

    fn with_services(dashboard: &mut Dashboard) {
        let mut stopped = service("redis", 6379, false);
        stopped.state = ServiceState::Stopped;
        dashboard.apply(Update::Services(vec![
            service("mysql", 3306, true),
            service("dbgate", 19000, true),
            stopped,
        ]));
    }

    /// Draws onto a terminal that only exists in memory, so the tests assert on
    /// what a user would see rather than on internals.
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

    /// The column a word starts at. `str::find` gives a byte offset, and the
    /// box-drawing characters in a border are three bytes each, so using it
    /// directly walks off the end of the buffer.
    fn column_of(line: &str, needle: &str) -> Option<u16> {
        let byte = line.find(needle)?;
        Some(line[..byte].chars().count() as u16)
    }

    fn screen(buffer: &Buffer) -> String {
        lines(buffer).join("\n")
    }

    /// The style of the cell at the start of the row whose text contains
    /// `needle`, taken inside the frame so the border is not what gets measured.
    fn row_style(buffer: &Buffer, needle: &str) -> Option<Style> {
        lines(buffer)
            .iter()
            .position(|line| line.contains(needle))
            .map(|y| buffer[(2, y as u16)].style())
    }

    #[test]
    fn projects_accumulate_as_they_arrive_rather_than_all_at_once() {
        let mut dashboard = Dashboard::new();
        assert_eq!(dashboard.rows_in(Pane::Projects), 0);
        dashboard.apply(Update::Project(project("alpha")));
        assert_eq!(dashboard.rows_in(Pane::Projects), 1);
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
            "moving past the end stops at the last row"
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
    fn each_pane_keeps_its_own_row_when_the_focus_moves_away_and_back() {
        let mut dashboard = with_projects(5);
        with_services(&mut dashboard);

        dashboard.move_selection(3);
        dashboard.focus_next();
        dashboard.move_selection(2);
        assert_eq!(dashboard.selected(), 2, "the services pane has its own row");

        dashboard.focus_on(Pane::Projects);
        assert_eq!(
            dashboard.selected(),
            3,
            "coming back to a list and finding it reset is how a dashboard loses your place"
        );
    }

    #[test]
    fn the_focus_cycles_both_ways() {
        let mut dashboard = Dashboard::new();
        assert_eq!(dashboard.focus(), Pane::Projects);
        dashboard.focus_next();
        dashboard.focus_next();
        assert_eq!(dashboard.focus(), Pane::Ports);
        dashboard.focus_next();
        assert_eq!(dashboard.focus(), Pane::Projects);
        dashboard.focus_previous();
        assert_eq!(dashboard.focus(), Pane::Ports);
    }

    #[test]
    fn a_selection_past_the_end_of_a_shorter_refresh_is_pulled_back_in() {
        let mut dashboard = Dashboard::new();
        with_services(&mut dashboard);
        dashboard.focus_on(Pane::Services);
        dashboard.move_selection(2);
        assert_eq!(dashboard.selected(), 2);

        dashboard.apply(Update::Services(vec![service("mysql", 3306, true)]));
        assert_eq!(
            dashboard.selected(),
            0,
            "a row number left pointing past a list that shrank would draw nothing"
        );
    }

    #[test]
    fn ports_lists_only_the_services_that_publish_one() {
        let mut dashboard = Dashboard::new();
        let mut quiet = service("internal", 0, false);
        quiet.port = None;
        dashboard.apply(Update::Services(vec![service("mysql", 3306, true), quiet]));
        assert_eq!(dashboard.rows_in(Pane::Services), 2);
        assert_eq!(dashboard.rows_in(Pane::Ports), 1);
    }

    #[test]
    fn all_three_panes_are_on_screen_at_once_on_a_wide_terminal() {
        let mut dashboard = with_projects(2);
        with_services(&mut dashboard);
        let view = screen(&drawn(&mut dashboard, 120, 20));

        assert!(view.contains("Projects"), "projects pane");
        assert!(view.contains("Services"), "services pane");
        assert!(view.contains("Ports"), "ports pane");
        assert!(
            view.contains("p0") && view.contains("mysql") && view.contains("3306"),
            "the point of three panes is seeing all three without pressing anything"
        );
    }

    #[test]
    fn a_narrow_terminal_shows_the_focused_pane_whole_instead_of_three_slivers() {
        let mut dashboard = with_projects(2);
        with_services(&mut dashboard);

        let narrow = screen(&drawn(&mut dashboard, 70, 20));
        assert!(narrow.contains("Projects"));
        assert!(
            !narrow.contains("Services"),
            "three panes on a 70 column terminal are three unreadable columns"
        );

        dashboard.focus_on(Pane::Services);
        let after = screen(&drawn(&mut dashboard, 70, 20));
        assert!(after.contains("Services") && after.contains("mysql"));
    }

    #[test]
    fn the_focused_pane_is_the_one_that_looks_focused() {
        let mut dashboard = with_projects(2);
        with_services(&mut dashboard);

        let buffer = drawn(&mut dashboard, 120, 20);
        let rows = lines(&buffer);
        let projects_border = rows
            .iter()
            .position(|line| line.contains("Projects"))
            .expect("projects frame");
        let services_border = rows
            .iter()
            .position(|line| line.contains("Services"))
            .expect("services frame");
        let projects_at = column_of(&rows[projects_border], "Projects").unwrap();
        let services_at = column_of(&rows[services_border], "Services").unwrap();

        assert_ne!(
            buffer[(projects_at, projects_border as u16)].style(),
            buffer[(services_at, services_border as u16)].style(),
            "if every pane looks the same, nothing says where the keys will land"
        );
    }

    #[test]
    fn the_current_row_is_drawn_differently_from_the_others() {
        let mut dashboard = with_projects(3);
        dashboard.move_selection(1);
        let buffer = drawn(&mut dashboard, 120, 20);

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
        assert!(
            screen(&drawn(&mut dashboard, 120, 20)).contains("p150"),
            "a selection the list has scrolled past is a selection nobody can see"
        );
    }

    #[test]
    fn a_list_that_fits_gets_no_scrollbar_and_a_longer_one_does() {
        let mut fits = with_projects(3);
        let mut overflows = with_projects(200);
        assert!(
            !screen(&drawn(&mut fits, 120, 20)).contains('█'),
            "a scrollbar on a list you can see all of takes a column and says nothing"
        );
        assert!(
            screen(&drawn(&mut overflows, 120, 20)).contains('█'),
            "without it there is no way to tell where in 200 rows you are"
        );
    }

    #[test]
    fn a_docker_failure_is_shown_as_a_failure_not_as_zero_services() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ServicesFailed("connection refused".to_string()));
        let view = screen(&drawn(&mut dashboard, 120, 20));
        assert!(
            view.contains("connection refused"),
            "the reason must reach the screen"
        );
        assert!(
            !view.contains("0 of 0 ready"),
            "an unreachable daemon must not read as a daemon with nothing running"
        );
    }

    #[test]
    fn each_pane_says_how_much_is_in_it_without_being_scrolled() {
        let mut dashboard = with_projects(3);
        with_services(&mut dashboard);
        let view = screen(&drawn(&mut dashboard, 120, 20));
        assert!(
            view.contains("3 of 3 examined"),
            "projects count and denominator"
        );
        assert!(view.contains("2 of 3 ready"), "services ready out of total");
        assert!(
            view.contains("2 of 3 answering"),
            "ports answering out of published"
        );
    }

    #[test]
    fn a_narrow_status_line_keeps_the_numbers_and_drops_the_hints() {
        let mut dashboard = with_projects(8);
        let wide = screen(&drawn(&mut dashboard, 120, 20));
        let narrow = screen(&drawn(&mut dashboard, 70, 14));

        assert!(wide.contains("8 projects") && wide.contains("q quit"));
        assert!(
            narrow.contains("8 projects"),
            "the counts are what the line is for"
        );
        assert!(
            !narrow.contains("q quit"),
            "hints that run into the counts leave neither readable"
        );
    }
    #[test]
    fn a_service_that_is_ready_is_not_drawn_like_one_that_is_stopped() {
        let mut dashboard = Dashboard::new();
        with_services(&mut dashboard);
        let buffer = drawn(&mut dashboard, 120, 20);
        let rows = lines(&buffer);

        let ready_row = rows
            .iter()
            .position(|l| l.contains("mysql"))
            .expect("mysql");
        let stopped_row = rows
            .iter()
            .position(|l| l.contains("redis"))
            .expect("redis");
        let ready_at = column_of(&rows[ready_row], "ready").expect("the word ready");
        let stopped_at = column_of(&rows[stopped_row], "stopped").expect("the word stopped");

        assert_ne!(
            buffer[(ready_at, ready_row as u16)].style().fg,
            buffer[(stopped_at, stopped_row as u16)].style().fg,
            "colour should carry the same distinction the word does"
        );
    }
}
