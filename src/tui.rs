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
use crate::listen::Listener;
use crate::memory;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Clear, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Table, TableState,
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
    ScanFailed {
        path: PathBuf,
        reason: String,
    },
    ScanFinished {
        scanned: usize,
    },
    Services(Vec<ServiceStatus>),
    /// Everything listening on this machine, docker or not. A separate fact
    /// from what docker publishes: the port you are looking for is usually
    /// held by a stray dev server, not a container.
    Ports(Vec<Listener>),
    PortsFailed(String),
    /// What the container host is costing, re-read on a timer.
    Memory(memory::Reading),
    ServicesFailed(String),
    /// What an action the user asked for is doing. Actions run on their own
    /// threads, so the only way they can report is the same way collectors do.
    Notice(Notice),
    /// One line a container wrote. It carries the container name so a stream
    /// still winding down after its view was closed cannot pour into the next.
    LogLine {
        container: String,
        line: String,
    },
}

/// A box laid over the screen: a list of labelled values with a title.
///
/// The settings screen was the first of these and is now one of three, along
/// with a project's toolchains and the routed hostnames. Anything that is read
/// rather than acted on belongs here instead of in a fourth pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub title: String,
    pub lines: Vec<(String, String)>,
    /// What to press to leave, and where to go to change what is shown.
    pub hint: String,
}

/// A line about something the user just asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    /// `None` while it is still happening. A spinner would say less than the
    /// word "starting" does.
    pub ok: Option<bool>,
}

impl Notice {
    pub fn working(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: None,
        }
    }

    pub fn done(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: Some(true),
        }
    }

    pub fn failed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: Some(false),
        }
    }
}

/// The log of one service, held while it is being watched.
///
/// A ring rather than a growing list: a chatty container would otherwise use
/// memory without limit for lines nobody will scroll back to.
pub struct LogView {
    container: String,
    lines: std::collections::VecDeque<String>,
    /// How far back from the newest line the view is. Zero follows the tail,
    /// which is what a log is usually opened for.
    scroll: usize,
}

impl LogView {
    /// Enough to scroll back through, small enough that a container writing
    /// thousands of lines a second cannot exhaust memory.
    const KEPT: usize = 2000;

    fn new(container: String) -> Self {
        Self {
            container,
            lines: std::collections::VecDeque::new(),
            scroll: 0,
        }
    }

    fn push(&mut self, line: String) {
        self.lines.push_back(line);
        if self.lines.len() > Self::KEPT {
            self.lines.pop_front();
        }
        // Scrolled back on purpose: hold that position instead of yanking the
        // reader to the bottom every time a line arrives.
        if self.scroll > 0 {
            self.scroll = (self.scroll + 1).min(self.lines.len().saturating_sub(1));
        }
    }

    pub fn container(&self) -> &str {
        &self.container
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The lines to show, oldest first, for a window this tall.
    fn window(&self, height: usize) -> Vec<&String> {
        let end = self.lines.len().saturating_sub(self.scroll);
        let start = end.saturating_sub(height);
        self.lines.range(start..end).collect()
    }
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
    /// The last thing an action said. Replaced rather than queued: what is
    /// happening now is what matters, and a backlog of old lines would bury it.
    notice: Option<Notice>,
    help: bool,
    /// The log being watched, when one is. While it is open it takes the whole
    /// screen: a log is read, not glanced at beside three other lists.
    logs: Option<LogView>,
    /// The settings in force, already turned into lines by the caller. The
    /// dashboard never reads configuration itself: it shows what it is told,
    /// which keeps it free of anything that touches a file.
    settings: Vec<(String, String)>,
    /// What is laid over the screen, when anything is.
    detail: Option<Detail>,
    /// A destructive action waiting for a yes. It takes the status line so it
    /// cannot be answered by a keystroke meant for something else.
    confirm: Option<String>,
    /// A line being typed: what it is for, and what has been typed so far.
    prompt: Option<(String, String)>,
    /// What the container host is costing this machine. Polled on its own
    /// timer, so a slow probe cannot hold up the rest of the screen.
    memory: memory::Reading,
    ports: Vec<Listener>,
    ports_error: Option<String>,
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
            notice: None,
            help: false,
            logs: None,
            settings: Vec::new(),
            detail: None,
            confirm: None,
            prompt: None,
            memory: memory::Reading::default(),
            ports: Vec::new(),
            ports_error: None,
        }
    }

    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Project(project) => self.projects.push(project),
            Update::Memory(reading) => self.memory = reading,
            Update::ScanFailed { path, reason } => self.scan_failures.push((path, reason)),
            Update::ScanFinished { scanned } => self.scanned = Some(scanned),
            Update::Services(services) => {
                self.services = services;
                self.services_error = None;
                self.clamp(Pane::Services);
                self.clamp(Pane::Ports);
            }
            Update::ServicesFailed(reason) => self.services_error = Some(reason),
            Update::Ports(ports) => {
                self.ports = ports;
                self.ports_error = None;
                self.clamp(Pane::Ports);
            }
            Update::PortsFailed(reason) => self.ports_error = Some(reason),
            Update::Notice(notice) => self.notice = Some(notice),
            Update::LogLine { container, line } => {
                // Only for the log actually open. A stream still winding down
                // after its view was closed would otherwise feed the next one.
                if let Some(view) = self.logs.as_mut() {
                    if view.container == container {
                        view.push(line);
                    }
                }
            }
        }
    }

    pub fn open_logs(&mut self, container: String) {
        self.logs = Some(LogView::new(container));
    }

    pub fn close_logs(&mut self) {
        self.logs = None;
    }

    pub fn logs(&self) -> Option<&LogView> {
        self.logs.as_ref()
    }

    /// Moves back through the log, or forward towards the newest line.
    pub fn scroll_logs(&mut self, delta: isize) {
        if let Some(view) = self.logs.as_mut() {
            let furthest = view.lines.len().saturating_sub(1) as isize;
            view.scroll = (view.scroll as isize - delta).clamp(0, furthest) as usize;
        }
    }

    /// Hands the dashboard the settings to show, already flattened to lines.
    pub fn set_settings(&mut self, settings: Vec<(String, String)>) {
        self.settings = settings;
    }

    /// Shows the settings, which are the one overlay the dashboard can build
    /// for itself because it was handed the lines at startup.
    pub fn toggle_settings(&mut self) {
        if self.detail.is_some() {
            self.detail = None;
            return;
        }
        self.detail = Some(Detail {
            title: "Settings".to_string(),
            lines: self.settings.clone(),
            hint: "adev config --edit to change · esc to close".to_string(),
        });
    }

    pub fn show_detail(&mut self, detail: Detail) {
        self.detail = Some(detail);
    }

    pub fn close_detail(&mut self) {
        self.detail = None;
    }

    pub fn showing_detail(&self) -> bool {
        self.detail.is_some()
    }

    /// Asks for a line of text: a database name, a file, a hostname.
    ///
    /// The dashboard holds the characters and nothing else. What the answer is
    /// for, and what happens when it arrives, stays with the caller - which is
    /// how a screen that knows nothing about databases can still ask for one.
    pub fn ask_for(&mut self, label: impl Into<String>) {
        self.prompt = Some((label.into(), String::new()));
    }

    pub fn prompting(&self) -> Option<&str> {
        self.prompt.as_ref().map(|(label, _)| label.as_str())
    }

    pub fn type_char(&mut self, c: char) {
        if let Some((_, typed)) = self.prompt.as_mut() {
            typed.push(c);
        }
    }

    pub fn backspace(&mut self) {
        if let Some((_, typed)) = self.prompt.as_mut() {
            typed.pop();
        }
    }

    /// Takes the answer and closes the prompt. An empty string is a real
    /// answer - the caller decides whether it will do - while `None` means
    /// there was nothing being asked.
    pub fn take_typed(&mut self) -> Option<String> {
        self.prompt.take().map(|(_, typed)| typed)
    }

    /// Throws the answer away. Escape must not hand back a half typed name
    /// that then gets used for something.
    pub fn cancel_prompt(&mut self) {
        self.prompt = None;
    }

    /// Asks a yes-or-no question that must be answered before anything else
    /// happens. Only for actions that cannot be undone.
    pub fn ask(&mut self, question: impl Into<String>) {
        self.confirm = Some(question.into());
    }

    pub fn confirming(&self) -> Option<&str> {
        self.confirm.as_deref()
    }

    pub fn dismiss(&mut self) {
        self.confirm = None;
    }

    pub fn toggle_help(&mut self) {
        self.help = !self.help;
    }

    pub fn showing_help(&self) -> bool {
        self.help
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// The project the cursor is on, when the projects pane has the focus.
    /// Actions read this rather than being told a name, so what happens is
    /// always what the highlighted row says.
    pub fn selected_project(&self) -> Option<&Project> {
        (self.focus == Pane::Projects)
            .then(|| self.projects.get(self.selected[Pane::Projects.index()]))
            .flatten()
    }

    /// The service the cursor is on.
    ///
    /// In the ports pane most rows have no container behind them - that is the
    /// point of listing every listener - so the answer there is whichever
    /// service published that port, and nothing when none did.
    pub fn selected_service(&self) -> Option<&ServiceStatus> {
        match self.focus {
            Pane::Services => self.services.get(self.selected[Pane::Services.index()]),
            Pane::Ports => {
                let port = self.selected_port()?.port;
                self.services.iter().find(|s| s.port == Some(port))
            }
            Pane::Projects => None,
        }
    }

    /// The listener the cursor is on, which exists whether or not docker knows
    /// anything about it.
    pub fn selected_port(&self) -> Option<&Listener> {
        match self.focus {
            Pane::Ports => self.ports.get(self.selected[Pane::Ports.index()]),
            _ => None,
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
            Pane::Ports => self.ports.len(),
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        if let Some(view) = &self.logs {
            draw_logs(frame, view);
            self.draw_overlays(frame);
            return;
        }

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

        self.draw_overlays(frame);
    }

    /// Whatever is layered over the screen. Settings first, so the key list
    /// opened on top of it is the one that closes first.
    fn draw_overlays(&self, frame: &mut Frame) {
        if let Some(detail) = &self.detail {
            draw_detail(frame, detail);
        }
        if self.help {
            draw_help(frame);
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
                vec!["PORT", "PROCESS", "SERVICE"],
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
            Pane::Ports if self.ports_error.is_some() => "unreadable".to_string(),
            Pane::Services => {
                let ready = self.services.iter().filter(|s| s.is_reachable()).count();
                format!("{ready} of {} ready", self.services.len())
            }
            Pane::Ports => {
                // How many are containers is the useful split here: the rest
                // are the ones nothing is managing for you.
                let docker = self
                    .ports
                    .iter()
                    .filter(|listener| self.services.iter().any(|s| s.port == Some(listener.port)))
                    .count();
                format!("{} listening · {docker} docker", self.ports.len())
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
        self.ports
            .iter()
            .map(|listener| {
                // Naming the container when there is one is most of the value:
                // it turns "something holds 3306" into "that is mysql".
                let service = self
                    .services
                    .iter()
                    .find(|s| s.port == Some(listener.port))
                    .map(|s| s.service.clone())
                    .unwrap_or_default();
                Row::new(vec![
                    Cell::from(listener.port.to_string()),
                    Cell::from(listener.process.clone().unwrap_or_else(|| "-".to_string()))
                        .style(Style::default().fg(paint::MUTED)),
                    Cell::from(service).style(Style::default().fg(paint::GOOD)),
                ])
            })
            .collect()
    }

    fn status_line(&self) -> Paragraph<'static> {
        // A question outranks everything. A collector finishing at the wrong
        // moment must not push it off the line the answer is typed at.
        if let Some(question) = &self.confirm {
            return Paragraph::new(Line::from(Span::styled(
                format!(" {question}  [y/N] "),
                Style::default().fg(paint::BAD).add_modifier(Modifier::BOLD),
            )));
        }
        // Next, whatever is being typed. A collector finishing mid-word must
        // not take the line the answer is going into.
        if let Some((label, typed)) = &self.prompt {
            return Paragraph::new(Line::from(vec![
                Span::styled(format!(" {label}: "), Style::default().fg(paint::HEADING)),
                Span::styled(typed.clone(), Style::default().add_modifier(Modifier::BOLD)),
                // Somewhere for the next character to land: a prompt with no
                // cursor reads as a message rather than as a question.
                Span::styled("█", Style::default().fg(paint::FOCUS)),
                Span::styled(
                    "   enter to accept · esc to cancel",
                    Style::default().fg(paint::MUTED),
                ),
            ]));
        }
        // What just happened takes the line while it is fresh. The counts are
        // still on every pane's frame, so nothing is actually lost.
        if let Some(notice) = &self.notice {
            let style = match notice.ok {
                None => Style::default().fg(paint::WAITING),
                Some(true) => Style::default().fg(paint::GOOD),
                Some(false) => Style::default().fg(paint::BAD),
            };
            return Paragraph::new(Line::from(Span::styled(format!(" {}", notice.text), style)));
        }

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

        // What the container host is costing, last of all: it is the number
        // you glance at rather than the one you came for.
        if let Some(bytes) = self.memory.guest_bytes {
            spans.push(Span::styled(format!("   ram {}", human_bytes(bytes)), dim));
        }
        if let Some((process, bytes)) = &self.memory.host {
            spans.push(Span::styled(
                format!(" · {process} {}", human_bytes(*bytes)),
                dim,
            ));
        }

        Paragraph::new(Line::from(spans))
    }
}

/// Bytes at the precision a glance can use: a footer that reads 1.43 GB when
/// the number is moving every few seconds is harder to read, not more accurate.
fn human_bytes(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else {
        format!("{:.0} MB", bytes / MB)
    }
}

/// Every key, on request. The status line has room for two of them, and a
/// dashboard with a dozen actions cannot teach them from a strip of text.
const KEYS: &[(&str, &str)] = &[
    ("tab / 1 2 3", "move between panes"),
    ("j k  ↑ ↓", "move within a pane"),
    ("s", "start the selected service"),
    ("x", "stop it"),
    ("S", "restart it"),
    ("o", "open whatever the focused row serves"),
    ("enter", "run the selected project"),
    ("l", "read the selected service's log"),
    ("t", "open a shell in a project, on its own toolchain"),
    ("e", "open the selected project's folder"),
    ("b", "back up the selected service's databases"),
    ("K", "end what holds the selected port"),
    ("v", "the toolchain versions a project resolves to"),
    ("d", "the hostnames the proxy serves"),
    ("A", "route a new hostname"),
    ("X", "stop routing one"),
    ("E", "dump one database to a file"),
    ("I", "load a dump into a database"),
    (".", "which .env a project uses, and switch it"),
    (":", "run one command in a project"),
    ("r", "refresh this pane"),
    ("R", "refresh everything"),
    ("g", "the settings in force, and where they live"),
    ("?", "this list"),
    ("q", "quit"),
];

/// A box in the middle, sized to its contents rather than to a percentage of
/// the screen, so it does not grow silly on a wide terminal.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The keys, kept to the right so the numbers on the left are read first.
fn key_hints() -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        "? keys · r refresh · q quit ",
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
        // Italic rather than another colour: absent is not a worse kind of
        // stopped, it is a service that was never created, and the row exists
        // to be acted on rather than worried about.
        (ServiceState::Absent, _) => Style::default()
            .fg(paint::MUTED)
            .add_modifier(Modifier::ITALIC),
    }
}

/// Draws the log of one service, whole. A log is read rather than glanced at,
/// so it takes the screen instead of squeezing beside the three lists.
fn draw_logs(frame: &mut Frame, view: &LogView) {
    let [body, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

    let inside = body.height.saturating_sub(2) as usize;
    let lines: Vec<Line<'static>> = view
        .window(inside)
        .into_iter()
        .map(|line| Line::from(line.clone()))
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(Style::default().fg(paint::FOCUS))
                .padding(Padding::horizontal(1))
                .title(Span::styled(
                    format!(" logs · {} ", view.container()),
                    Style::default()
                        .fg(paint::FOCUS)
                        .add_modifier(Modifier::BOLD),
                ))
                .title_bottom(Span::styled(
                    if view.is_empty() {
                        " waiting for output… ".to_string()
                    } else {
                        format!(" {} lines ", view.len())
                    },
                    Style::default().fg(paint::MUTED),
                )),
        ),
        body,
    );

    let position = if view.scroll == 0 {
        Span::styled(" following", Style::default().fg(paint::GOOD))
    } else {
        // Said plainly: a log that has stopped moving because the reader
        // scrolled looks exactly like a service that has gone quiet.
        Span::styled(
            format!(" {} lines back — j to follow again", view.scroll),
            Style::default().fg(paint::WAITING),
        )
    };
    frame.render_widget(Paragraph::new(Line::from(position)), status);
}

/// Draws the settings in force over whatever is behind them.
///
/// Read-only on purpose. The configuration file carries comments explaining
/// each choice, and rewriting it from a form would throw those away every time
/// somebody changed a number - so this says what is in force and where to go
/// and change it.
fn draw_detail(frame: &mut Frame, detail: &Detail) {
    let width = 74;
    let height = (detail.lines.len() as u16 + 2).min(frame.area().height);
    let area = centred(frame.area(), width, height);

    let rows: Vec<Row<'static>> = detail
        .lines
        .iter()
        .map(|(key, value)| {
            Row::new(vec![
                Cell::from(key.clone()).style(Style::default().fg(paint::MUTED)),
                Cell::from(value.clone()),
            ])
        })
        .collect();

    frame.render_widget(Clear, area);
    frame.render_widget(
        Table::new(rows, [Constraint::Length(20), Constraint::Fill(1)])
            .block(
                Block::bordered()
                    .border_style(Style::default().fg(paint::FOCUS))
                    .padding(Padding::horizontal(1))
                    .title(Span::styled(
                        format!(" {} ", detail.title),
                        Style::default()
                            .fg(paint::FOCUS)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .title_bottom(Span::styled(
                        format!(" {} ", detail.hint),
                        Style::default().fg(paint::MUTED),
                    )),
            )
            .column_spacing(2),
        area,
    );
}

/// Draws the key list over whatever is behind it.
fn draw_help(frame: &mut Frame) {
    // Two columns, because one is now taller than a 24-row terminal and a key
    // list that scrolls off the screen is not a key list.
    let half = KEYS.len().div_ceil(2);
    let rows: Vec<Row<'static>> = (0..half)
        .map(|row| {
            let mut cells = vec![
                Cell::from(KEYS[row].0).style(Style::default().fg(paint::FOCUS)),
                Cell::from(KEYS[row].1),
            ];
            match KEYS.get(row + half) {
                Some((key, meaning)) => {
                    cells.push(Cell::from(*key).style(Style::default().fg(paint::FOCUS)));
                    cells.push(Cell::from(*meaning));
                }
                // An odd number of keys leaves the last cell empty rather than
                // wrapping one entry onto a row of its own.
                None => cells.extend([Cell::from(""), Cell::from("")]),
            }
            Row::new(cells)
        })
        .collect();

    let width = 104;
    let height = half as u16 + 2;
    let area = centred(frame.area(), width, height);

    // Cleared first: without it the list underneath shows through the gaps
    // between words and neither is readable.
    frame.render_widget(Clear, area);
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(13),
                Constraint::Fill(1),
                Constraint::Length(7),
                Constraint::Fill(1),
            ],
        )
        .block(
            Block::bordered()
                .border_style(Style::default().fg(paint::FOCUS))
                .padding(Padding::horizontal(1))
                .title(Span::styled(
                    " Keys ",
                    Style::default()
                        .fg(paint::FOCUS)
                        .add_modifier(Modifier::BOLD),
                ))
                .title_bottom(Span::styled(
                    " ? or esc to close ",
                    Style::default().fg(paint::MUTED),
                )),
        )
        .column_spacing(2),
        area,
    );
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

    #[test]
    fn the_footer_shows_what_the_container_host_costs() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ScanFinished { scanned: 0 });
        dashboard.apply(Update::Services(Vec::new()));
        dashboard.apply(Update::Memory(memory::Reading {
            guest_bytes: Some(1423 * 1024 * 1024),
            host: Some(("vmmemWSL".to_string(), 855_312 * 1024)),
        }));

        let buffer = drawn(&mut dashboard, 160, 20);
        let footer = lines(&buffer).pop().expect("a status line");
        assert!(footer.contains("ram 1.4 GB"), "got {footer:?}");
        assert!(footer.contains("vmmemWSL 835 MB"), "got {footer:?}");
    }

    #[test]
    fn a_machine_with_nothing_to_measure_shows_no_memory_at_all() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ScanFinished { scanned: 0 });
        dashboard.apply(Update::Services(Vec::new()));

        let buffer = drawn(&mut dashboard, 160, 20);
        let footer = lines(&buffer).pop().expect("a status line");
        assert!(
            !footer.contains("ram"),
            "an unmeasured number must not read as zero; got {footer:?}"
        );
    }

    #[test]
    fn bytes_are_shown_at_the_precision_a_glance_can_use() {
        assert_eq!(human_bytes(855_312 * 1024), "835 MB");
        assert_eq!(human_bytes(1423 * 1024 * 1024), "1.4 GB");
        assert_eq!(human_bytes(0), "0 MB");
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

    fn listener(port: u16, process: &str, pid: u32) -> Listener {
        Listener {
            port,
            pid: Some(pid),
            process: Some(process.to_string()),
        }
    }

    #[test]
    fn the_ports_pane_lists_every_listener_not_only_the_docker_ones() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::Services(vec![service("mysql", 3306, true)]));
        dashboard.apply(Update::Ports(vec![
            listener(3306, "wslrelay.exe", 17336),
            listener(5173, "node.exe", 900),
            listener(22, "sshd.exe", 4764),
        ]));

        assert_eq!(
            dashboard.rows_in(Pane::Ports),
            3,
            "the pane is called Ports; a stray dev server is the usual reason to look at it"
        );
    }

    #[test]
    fn a_port_that_belongs_to_a_container_still_reaches_that_service() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::Services(vec![service("mysql", 3306, true)]));
        dashboard.apply(Update::Ports(vec![
            listener(22, "sshd.exe", 4764),
            listener(3306, "wslrelay.exe", 17336),
        ]));
        dashboard.focus_on(Pane::Ports);
        dashboard.move_selection(1);

        assert_eq!(
            dashboard.selected_service().map(|s| s.service.as_str()),
            Some("mysql"),
            "start, stop and logs must keep working from this pane"
        );
    }

    #[test]
    fn a_port_no_container_owns_resolves_to_no_service() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::Services(vec![service("mysql", 3306, true)]));
        dashboard.apply(Update::Ports(vec![listener(5173, "node.exe", 900)]));
        dashboard.focus_on(Pane::Ports);

        assert!(
            dashboard.selected_service().is_none(),
            "a stray dev server has no container to stop"
        );
        assert_eq!(dashboard.selected_port().map(|l| l.port), Some(5173));
    }

    #[test]
    fn a_stray_dev_server_is_on_screen_with_the_process_holding_it() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ScanFinished { scanned: 0 });
        dashboard.apply(Update::Services(vec![service("mysql", 3306, true)]));
        dashboard.apply(Update::Ports(vec![
            listener(3306, "wslrelay.exe", 17336),
            listener(5173, "node.exe", 900),
        ]));

        let view = screen(&drawn(&mut dashboard, 140, 24));
        assert!(view.contains("5173"), "the stray port itself");
        assert!(view.contains("node.exe"), "what is holding it");
        assert!(
            view.contains("mysql"),
            "and the container name where there is one"
        );
        assert!(
            view.contains("2 listening"),
            "the count belongs on the frame; got {view}"
        );
    }

    #[test]
    fn a_question_takes_the_status_line_until_it_is_answered() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ScanFinished { scanned: 0 });
        dashboard.apply(Update::Notice(Notice::done("started mysql")));
        dashboard.ask("kill node.exe (pid 900) on 5173?");

        let footer = lines(&drawn(&mut dashboard, 140, 20)).pop().unwrap();
        assert!(footer.contains("kill node.exe"), "got {footer:?}");
        assert!(
            !footer.contains("started mysql"),
            "a question that shares its line with an old notice can be answered by accident"
        );
        assert_eq!(
            dashboard.confirming(),
            Some("kill node.exe (pid 900) on 5173?")
        );

        dashboard.dismiss();
        assert_eq!(dashboard.confirming(), None);
    }

    #[test]
    fn an_arriving_notice_cannot_replace_a_question_that_is_still_open() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ScanFinished { scanned: 0 });
        dashboard.ask("kill node.exe (pid 900) on 5173?");
        // A collector finishing at the wrong moment must not turn a pending
        // "kill this?" into something the next keystroke answers blind.
        dashboard.apply(Update::Notice(Notice::done("rereading ports")));

        let footer = lines(&drawn(&mut dashboard, 140, 20)).pop().unwrap();
        assert!(footer.contains("kill node.exe"), "got {footer:?}");
    }

    #[test]
    fn what_is_typed_appears_on_the_status_line_and_comes_back_on_enter() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ScanFinished { scanned: 0 });
        dashboard.ask_for("database to export");

        for c in "shop_db".chars() {
            dashboard.type_char(c);
        }
        dashboard.backspace();

        let footer = lines(&drawn(&mut dashboard, 140, 20)).pop().unwrap();
        assert!(footer.contains("database to export"), "got {footer:?}");
        assert!(footer.contains("shop_d"), "what has been typed so far");

        assert_eq!(dashboard.take_typed(), Some("shop_d".to_string()));
        assert!(
            dashboard.prompting().is_none(),
            "taking the answer closes the prompt"
        );
    }

    #[test]
    fn a_cancelled_prompt_yields_nothing_at_all() {
        let mut dashboard = Dashboard::new();
        dashboard.ask_for("database to export");
        dashboard.type_char('x');
        dashboard.cancel_prompt();

        assert!(dashboard.prompting().is_none());
        assert_eq!(
            dashboard.take_typed(),
            None,
            "a cancelled prompt must not hand back what was half typed"
        );
    }

    #[test]
    fn an_arriving_notice_cannot_overwrite_what_is_being_typed() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ScanFinished { scanned: 0 });
        dashboard.ask_for("database to export");
        dashboard.type_char('s');
        dashboard.apply(Update::Notice(Notice::done("rereading services")));

        let footer = lines(&drawn(&mut dashboard, 140, 20)).pop().unwrap();
        assert!(
            footer.contains("database to export"),
            "a collector must not take the line from under a half typed answer; got {footer:?}"
        );
    }

    #[test]
    fn an_empty_answer_is_still_an_answer_the_caller_can_refuse() {
        let mut dashboard = Dashboard::new();
        dashboard.ask_for("database to export");
        assert_eq!(
            dashboard.take_typed(),
            Some(String::new()),
            "enter on an empty prompt is a decision, not a cancellation"
        );
    }

    #[test]
    fn a_detail_overlay_carries_its_own_title_and_lines() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::ScanFinished { scanned: 0 });
        dashboard.show_detail(Detail {
            title: "shop-web".to_string(),
            lines: vec![(
                "php".to_string(),
                "7.4.33  asked for by the project".to_string(),
            )],
            hint: "esc to close".to_string(),
        });

        let view = screen(&drawn(&mut dashboard, 140, 24));
        assert!(view.contains("shop-web"), "the title names what is shown");
        assert!(view.contains("7.4.33"), "got {view}");
        assert!(dashboard.showing_detail());

        dashboard.close_detail();
        assert!(!dashboard.showing_detail());
    }

    #[test]
    fn the_ports_pane_is_empty_until_its_own_collector_reports() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::Services(vec![service("mysql", 3306, true)]));
        assert_eq!(
            dashboard.rows_in(Pane::Ports),
            0,
            "docker publishing a port is not the same fact as something listening on it"
        );
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
            view.contains("0 listening"),
            "the ports pane counts listeners, and none have been reported here"
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

    #[test]
    fn what_an_action_is_doing_takes_the_status_line_while_it_is_fresh() {
        let mut dashboard = with_projects(3);
        assert!(screen(&drawn(&mut dashboard, 120, 20)).contains("3 projects"));

        dashboard.apply(Update::Notice(Notice::working("starting mysql")));
        let view = screen(&drawn(&mut dashboard, 120, 20));
        assert!(view.contains("starting mysql"));
        assert!(
            view.contains("3 of 3 examined"),
            "the counts move to the frames rather than disappearing"
        );
    }

    #[test]
    fn the_newest_notice_replaces_the_last_rather_than_queueing_behind_it() {
        let mut dashboard = Dashboard::new();
        dashboard.apply(Update::Notice(Notice::working("starting mysql")));
        dashboard.apply(Update::Notice(Notice::done("started mysql")));
        assert_eq!(
            dashboard.notice().map(|n| n.text.as_str()),
            Some("started mysql")
        );
        assert_eq!(dashboard.notice().and_then(|n| n.ok), Some(true));
    }

    #[test]
    fn an_action_reads_the_row_the_cursor_is_on_not_a_name_it_was_told() {
        let mut dashboard = with_projects(3);
        with_services(&mut dashboard);

        dashboard.move_selection(2);
        assert_eq!(
            dashboard.selected_project().map(|p| p.name.as_str()),
            Some("p2")
        );
        assert!(
            dashboard.selected_service().is_none(),
            "a service action from the projects pane would act on something unseen"
        );

        dashboard.focus_on(Pane::Services);
        dashboard.move_selection(1);
        assert_eq!(
            dashboard.selected_service().map(|s| s.service.as_str()),
            Some("dbgate")
        );
        assert!(dashboard.selected_project().is_none());
    }

    #[test]
    fn the_ports_pane_selects_by_its_own_list_not_the_services_one() {
        let mut dashboard = Dashboard::new();
        let mut quiet = service("internal", 0, false);
        quiet.port = None;
        // The unpublished one sits first, so a shared row number would pick it.
        dashboard.apply(Update::Services(vec![quiet, service("mysql", 3306, true)]));
        dashboard.apply(Update::Ports(vec![listener(3306, "wslrelay.exe", 17336)]));

        dashboard.focus_on(Pane::Ports);
        assert_eq!(
            dashboard.selected_service().map(|s| s.service.as_str()),
            Some("mysql"),
            "row 0 of ports is a listener, resolved to whichever service published it"
        );
    }

    #[test]
    fn the_key_list_is_available_on_request_rather_than_crammed_into_one_line() {
        let mut dashboard = with_projects(3);
        let quiet = screen(&drawn(&mut dashboard, 120, 24));
        assert!(!quiet.contains("start the selected service"));
        assert!(quiet.contains("? keys"), "the way in has to be visible");

        dashboard.toggle_help();
        let helping = screen(&drawn(&mut dashboard, 120, 24));
        assert!(helping.contains("start the selected service"));
        assert!(helping.contains("run the selected project"));

        dashboard.toggle_help();
        assert!(!screen(&drawn(&mut dashboard, 120, 24)).contains("start the selected service"));
    }

    #[test]
    fn the_key_list_covers_what_it_draws_rather_than_showing_through_it() {
        let mut dashboard = with_projects(30);
        dashboard.toggle_help();
        let lines = lines(&drawn(&mut dashboard, 120, 24));
        let overlay = lines
            .iter()
            .find(|line| line.contains("start the selected service"))
            .expect("the key list");
        assert!(
            !overlay.contains("p1"),
            "a row from the list underneath showing through would make both unreadable"
        );
    }

    #[test]
    fn a_log_takes_the_whole_screen_rather_than_a_corner_of_it() {
        let mut dashboard = with_projects(3);
        with_services(&mut dashboard);
        assert!(screen(&drawn(&mut dashboard, 120, 20)).contains("Projects"));

        dashboard.open_logs("mysql-1".to_string());
        let view = screen(&drawn(&mut dashboard, 120, 20));
        assert!(view.contains("logs · mysql-1"));
        assert!(
            !view.contains("PROJECT"),
            "a log is read, not glanced at beside three other lists"
        );

        dashboard.close_logs();
        assert!(screen(&drawn(&mut dashboard, 120, 20)).contains("PROJECT"));
    }

    #[test]
    fn lines_from_a_log_that_was_closed_do_not_pour_into_the_next_one() {
        let mut dashboard = Dashboard::new();
        dashboard.open_logs("mysql-1".to_string());
        dashboard.apply(Update::LogLine {
            container: "redis-1".to_string(),
            line: "from the wrong container".to_string(),
        });
        dashboard.apply(Update::LogLine {
            container: "mysql-1".to_string(),
            line: "ready for connections".to_string(),
        });

        let view = screen(&drawn(&mut dashboard, 120, 20));
        assert!(view.contains("ready for connections"));
        assert!(
            !view.contains("from the wrong container"),
            "a stream still winding down would otherwise feed the log after it"
        );
    }

    #[test]
    fn a_log_keeps_a_bounded_history_rather_than_growing_forever() {
        let mut dashboard = Dashboard::new();
        dashboard.open_logs("chatty".to_string());
        for index in 0..2500 {
            dashboard.apply(Update::LogLine {
                container: "chatty".to_string(),
                line: format!("line {index}"),
            });
        }
        assert_eq!(
            dashboard.logs().map(|view| view.len()),
            Some(2000),
            "a container writing thousands of lines a second must not exhaust memory"
        );
    }

    #[test]
    fn scrolling_back_holds_its_place_while_new_lines_arrive() {
        let mut dashboard = Dashboard::new();
        dashboard.open_logs("app".to_string());
        for index in 0..50 {
            dashboard.apply(Update::LogLine {
                container: "app".to_string(),
                line: format!("line {index}"),
            });
        }

        dashboard.scroll_logs(-20);
        let before = screen(&drawn(&mut dashboard, 120, 12));
        assert!(
            before.contains("lines back"),
            "the view says it is not following"
        );

        dashboard.apply(Update::LogLine {
            container: "app".to_string(),
            line: "line 50".to_string(),
        });
        assert!(
            !screen(&drawn(&mut dashboard, 120, 12)).contains("line 50"),
            "a reader who scrolled back should not be yanked to the bottom"
        );
    }

    #[test]
    fn following_is_the_default_and_says_so() {
        let mut dashboard = Dashboard::new();
        dashboard.open_logs("app".to_string());
        dashboard.apply(Update::LogLine {
            container: "app".to_string(),
            line: "started".to_string(),
        });
        let view = screen(&drawn(&mut dashboard, 120, 12));
        assert!(view.contains("following") && view.contains("started"));
    }

    #[test]
    fn the_settings_in_force_can_be_looked_at_from_the_dashboard() {
        let mut dashboard = with_projects(3);
        dashboard.set_settings(vec![
            ("file".to_string(), "C:/Users/x/aether.toml".to_string()),
            ("toolchain.php".to_string(), "4 installed".to_string()),
        ]);

        let quiet = screen(&drawn(&mut dashboard, 120, 24));
        assert!(!quiet.contains("aether.toml"));

        dashboard.toggle_settings();
        let showing = screen(&drawn(&mut dashboard, 120, 24));
        assert!(showing.contains("Settings"));
        assert!(
            showing.contains("aether.toml"),
            "where the settings came from is the thing people are looking for"
        );
        assert!(showing.contains("4 installed"));
        assert!(
            showing.contains("adev config --edit"),
            "showing settings with no way to change them is a dead end"
        );

        dashboard.toggle_settings();
        assert!(!screen(&drawn(&mut dashboard, 120, 24)).contains("Settings"));
    }

    #[test]
    fn the_settings_can_be_read_while_a_log_is_open() {
        let mut dashboard = Dashboard::new();
        dashboard.set_settings(vec![("file".to_string(), "somewhere.toml".to_string())]);
        dashboard.open_logs("mysql-1".to_string());
        dashboard.toggle_settings();
        assert!(screen(&drawn(&mut dashboard, 120, 20)).contains("somewhere.toml"));
    }
}
