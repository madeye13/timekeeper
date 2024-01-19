use crossterm_027 as crossterm;
use std::{
    cmp::max,
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::bail;
use chrono::{Local, NaiveDate};
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use futures_timer::Delay;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use regex::{Regex, RegexBuilder};
use serde::{de, Deserialize, Serialize};
use std::collections::{
    hash_map::Entry::{Occupied, Vacant},
    BTreeMap, HashMap,
};
use timeflippers::{
    timeflip::{Entry, Event as TimeEvent},
    BluetoothSession, Config, Facet, TimeFlip,
};
use tokio::{fs, select};
use tui_textarea::{Input, Key, TextArea};

use timekeeper::booker;

struct DurationView<'a>(&'a Duration);

impl<'a> fmt::Display for DurationView<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let seconds = self.0.as_secs() % 60;
        let minutes = (self.0.as_secs() / 60) % 60;
        let hours = self.0.as_secs() / 3600;

        let s = format!("{hours:02}:{minutes:02}:{seconds:02}");
        f.pad(&s)
    }
}

struct StatefulList<T> {
    state: ListState,
    items: Vec<T>,
}

impl<T> StatefulList<T> {
    fn with_items(items: Vec<T>, selection: Option<usize>) -> StatefulList<T> {
        let mut list_state = ListState::default();
        list_state.select(selection.or(Some(0)));
        StatefulList {
            state: list_state,
            items,
        }
    }

    fn next(&mut self) {
        if self.items.is_empty() {
            self.state.select(None);
            return;
        }
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        if self.items.is_empty() {
            self.state.select(None);
            return;
        }
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.items.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn remove(&mut self) {
        self.state.selected().map(|i| {
            self.items.remove(i);
            if self.items.is_empty() {
                self.state.select(None);
            } else if i >= self.items.len() {
                self.state.select(Some(self.items.len()));
            } else {
                self.state.select(Some(i));
            }
        });
    }

    fn selected(&self) -> Option<&T> {
        self.state.selected().map(|i| self.items.get(i)).flatten()
    }
}

struct App {
    items: StatefulList<u32>,
    entries: HashMap<u32, MyEntry>,
    show_invisible: bool,
}

impl App {
    fn new_from_entries(entries: Vec<MyEntry>) -> App {
        let map = entries.iter().map(|e| (e.entry.id, e.clone())).collect();
        let entry_ids = entries
            .iter()
            .filter_map(|e| {
                if e.deleted {
                    return None;
                }
                if e.visible && e.entry.duration > Duration::from_secs(30) {
                    Some(e.entry.id)
                } else {
                    None
                }
            })
            .collect();
        App {
            items: StatefulList::with_items(entry_ids, None),
            entries: map,
            show_invisible: false,
        }
    }

    fn update_entry_list(&mut self) {
        let mut new_items: Vec<u32> = self
            .entries
            .values()
            .filter_map(|e| {
                if e.deleted {
                    return None;
                }
                if e.entry.duration > Duration::from_secs(30) {
                    match (e.visible, self.show_invisible) {
                        (true, _) | (false, true) => Some(e.entry.id),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect();
        new_items.sort();
        let selection = self
            .items
            .selected()
            .map(|currently_selected| new_items.iter().position(|e| e == currently_selected))
            .flatten();
        self.items = StatefulList::with_items(new_items, selection);
    }

    fn toggle_visibility(&mut self) {
        self.show_invisible = !self.show_invisible;
        self.update_entry_list();
    }

    fn get_available_dates(&self) -> Vec<NaiveDate> {
        let mut available_dates = self
            .entries
            .values()
            .filter_map(|e| {
                if !e.deleted && e.entry.duration > Duration::from_secs(30) {
                    Some(
                        e.entry
                            .time
                            .with_timezone(&Local::now().timezone())
                            .date_naive(),
                    )
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        available_dates.sort();
        available_dates.dedup();
        available_dates
    }
}

#[derive(Parser)]
#[clap(about)]
struct Options {
    #[arg(help = "path to the timeflip.toml file")]
    config: PathBuf,
    #[arg(help = "read events from and write new events to file")]
    persistent_file: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let opt = Options::parse();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run(&mut terminal, opt).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MyEntry {
    #[serde(flatten)]
    entry: Entry,
    #[serde(default)]
    description: Vec<String>,
    #[serde(default)]
    visible: bool,
    #[serde(default)]
    deleted: bool,
}

async fn load_history(persistent_file: &PathBuf) -> anyhow::Result<(u32, Vec<MyEntry>)> {
    match fs::read_to_string(persistent_file).await {
        Ok(s) => {
            let mut entries: Vec<MyEntry> = serde_json::from_str(&s)?;
            entries.sort_by(|a, b| a.entry.id.cmp(&b.entry.id));
            Ok((entries.last().map(|e| e.entry.id).unwrap_or(0), entries))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((0, vec![])),
        Err(e) => Err(e.into()),
    }
}

async fn persist_history(persistent_file: &PathBuf, entries: &[MyEntry]) -> anyhow::Result<()> {
    match serde_json::to_vec_pretty(&entries) {
        Ok(json) => {
            if let Err(e) = fs::write(&persistent_file, json).await {
                bail!(
                    "cannot update entries file {}: {e}",
                    persistent_file.display()
                );
            }
        }
        Err(e) => bail!(
            "cannot update entries file {}: {e}",
            persistent_file.display()
        ),
    }
    Ok(())
}

fn deserialize_sides_jira_matches<'de, D>(
    deserializer: D,
) -> Result<HashMap<Facet, Regex>, D::Error>
where
    D: de::Deserializer<'de>,
{
    #[derive(Debug, PartialEq, Eq, Deserialize)]
    struct SidesJiraMatching {
        facet: Facet,
        jira_summary_regex: String,
    }
    let sides = Vec::<SidesJiraMatching>::deserialize(deserializer)?;
    let mapped: Result<HashMap<Facet, Regex>, D::Error> = sides
        .into_iter()
        .map(|sides| {
            let regex = RegexBuilder::new(&sides.jira_summary_regex)
                .case_insensitive(true)
                .build()
                .map_err(de::Error::custom)?;
            Ok((sides.facet, regex))
        })
        .collect();
    mapped
}

#[derive(Debug, Deserialize)]
struct MyConfig {
    #[serde(flatten)]
    timeflip: Config,
    jira_user: String,
    jira_token: String,
    jira_base_url: String,
    jira_board_id: String,
    jira_jql_issue_filter: String,
    #[serde(deserialize_with = "deserialize_sides_jira_matches")]
    sides_jira_matching: HashMap<Facet, Regex>,
}

async fn read_config(path: impl AsRef<Path>) -> anyhow::Result<MyConfig> {
    let toml = fs::read_to_string(path).await?;
    let config: MyConfig = toml::from_str(&toml)?;
    Ok(config)
}

fn facet_name(facet: &Facet, config: &Config) -> String {
    config.sides[facet.index_zero()]
        .name
        .clone()
        .unwrap_or(facet.to_string())
}

fn longest_facet_name(config: &Config) -> usize {
    config
        .sides
        .iter()
        .map(|side| side.name.clone().unwrap_or(side.facet.to_string()).len())
        .max()
        .unwrap_or_default()
}

#[derive(Eq, Hash, PartialEq, Ord, PartialOrd)]
enum BookingType {
    Jira(String),
    Unknown(Facet),
}

struct Booking {
    based_on_entries: Vec<u32>,
    duration: Duration,
}

struct BookingDate {
    date: NaiveDate,
    total_duration: Duration,
    jira_data: HashMap<String, booker::Fields>,
    suggested_bookings: BTreeMap<BookingType, Booking>,
}

enum State {
    Selecting,
    Editing,
    Paused,
    Booking {
        date_selection_list: StatefulList<NaiveDate>,
        booking_date: Option<BookingDate>,
    },
}

impl State {
    fn get_description(&self) -> String {
        match self {
            Self::Selecting => {
                String::from("[Up/Down] Move, [->] Edit, [p] Pause, [d] Done, [t] Toggle Visibility, [s] Sync, [q] Quit")
            }
            Self::Editing => String::from("[Esc] Finish editing"),
            Self::Paused => String::from("[p] Unpause"),
            Self::Booking {..} => String::from("[Esc] Finish booking"),
        }
    }
}

async fn get_bookings_for_date(app: &App, config: &MyConfig, date: &NaiveDate) -> BookingDate {
    let mut queried_issues: HashMap<String, booker::Fields> = booker::get_sprint_issues_for_date(
        &config.jira_base_url,
        &config.jira_board_id,
        &config.jira_user,
        &config.jira_token,
        date,
        &config.jira_jql_issue_filter,
    )
    .await
    .expect("query failed")
    .into_iter()
    .map(|issue| (issue.key, issue.fields))
    .collect();
    let mut missing_issues = vec![];
    let mut total_duration = Duration::new(0, 0);
    let date_entries = app
        .entries
        .values()
        .filter_map(|e| {
            if !e.deleted
                && e.entry.duration > Duration::from_secs(30)
                && &e
                    .entry
                    .time
                    .with_timezone(&Local::now().timezone())
                    .date_naive()
                    == date
            {
                total_duration += e.entry.duration;
                let re = RegexBuilder::new(r"CPS\-[0-9]+|IN\-[0-9]+|SYFU\-[0-9]+|SYMX\-[0-9]+")
                    .case_insensitive(true)
                    .build()
                    .unwrap();
                let issues: Vec<String> = e
                    .description
                    .iter()
                    .filter_map(|line| {
                        let found: Vec<String> = re
                            .find_iter(line)
                            .map(|c| c.as_str().to_string().to_uppercase())
                            .collect();
                        if found.is_empty() {
                            None
                        } else {
                            Some(found)
                        }
                    })
                    .flatten()
                    .collect();
                if issues.is_empty() {
                    let found_entry: Option<String> = config
                        .sides_jira_matching
                        .get(&e.entry.facet)
                        .map(|regex| {
                            queried_issues.iter().find_map(|(id, fields)| {
                                regex
                                    .find(&fields.summary.to_lowercase())
                                    .map(|_| id.clone())
                            })
                        })
                        .flatten();
                    let booking_type = match found_entry {
                        Some(id) => BookingType::Jira(id),
                        None => BookingType::Unknown(e.entry.facet.clone()),
                    };
                    Some(vec![(
                        booking_type,
                        Booking {
                            based_on_entries: vec![e.entry.id],
                            duration: e.entry.duration,
                        },
                    )])
                } else {
                    let n_issues: u32 = issues.len().try_into().expect("can't fit u32");
                    Some(
                        issues
                            .into_iter()
                            .map(|issue| {
                                if !queried_issues.contains_key(&issue) {
                                    missing_issues.push(issue.clone());
                                }
                                (
                                    BookingType::Jira(issue.clone()),
                                    Booking {
                                        based_on_entries: vec![e.entry.id],
                                        duration: e.entry.duration / n_issues,
                                    },
                                )
                            })
                            .collect(),
                    )
                }
            } else {
                None
            }
        })
        .flatten()
        .collect::<Vec<(BookingType, Booking)>>();

    let mut accumulated_bookings: BTreeMap<BookingType, Booking> = BTreeMap::new();

    date_entries
        .into_iter()
        .for_each(|(booking_type, booking)| {
            accumulated_bookings
                .entry(booking_type)
                .and_modify(|e| {
                    e.based_on_entries
                        .extend_from_slice(&booking.based_on_entries);
                    e.duration += booking.duration;
                })
                .or_insert(booking);
        });

    queried_issues.extend(
        booker::get_issues(
            &config.jira_base_url,
            &config.jira_user,
            &config.jira_token,
            missing_issues,
        )
        .await
        .into_iter()
        .map(|issue| (issue.key, issue.fields)),
    );
    BookingDate {
        date: date.clone(),
        total_duration,
        jira_data: queried_issues,
        suggested_bookings: accumulated_bookings,
    }
}

async fn run<B: Backend>(terminal: &mut Terminal<B>, opt: Options) -> anyhow::Result<()> {
    terminal.draw(|f| show_loading_window(f))?;
    let config = read_config(opt.config).await?;
    let (mut last_seen, entries) = load_history(&opt.persistent_file).await?;

    let (mut bg_task, session) = BluetoothSession::new().await?;
    let timeflip = TimeFlip::connect(&session, Some(config.timeflip.password)).await?;

    let mut app = App::new_from_entries(entries);

    let update: Vec<Entry> = timeflip
        .read_history_since(last_seen)
        .await?
        .into_iter()
        .collect();
    for entry in update {
        last_seen = max(entry.id, last_seen);
        match app.entries.entry(entry.id) {
            Vacant(v) => {
                v.insert(MyEntry {
                    entry,
                    description: vec![],
                    visible: true,
                    deleted: false,
                });
            }
            Occupied(mut o) => {
                o.get_mut().entry = entry;
            }
        }
    }
    app.update_entry_list();

    let mut textarea = if let Some(selected) = &app.items.selected() {
        let text = app
            .entries
            .get(selected)
            .expect("must be present")
            .description
            .clone();
        TextArea::new(text.to_vec())
    } else {
        TextArea::default()
    };

    let mut state = State::Selecting;
    let mut reader = EventStream::new();

    timeflip.subscribe_double_tap().await?;
    timeflip.subscribe_facet().await?;
    let mut stream = timeflip.event_stream().await?;

    loop {
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title("Additional information"),
        );
        terminal.draw(|f| ui(f, &mut app, &textarea, &mut state, &config.timeflip))?;
        let delay = Delay::new(Duration::from_millis(1_000));
        select! {
            event = stream.next() => {
                match event {
                    Some(TimeEvent::DoubleTap { pause, .. }) => {
                        match state {
                            State::Paused => {
                                if !pause {
                                    state = State::Selecting;
                                }
                            }
                            _ => {
                                if pause {
                                    state = State::Paused;
                                }
                            }
                        }
                    },
                    Some(TimeEvent::Facet(_facet)) => {
                        if matches!(state, State::Paused) {
                            state = State::Selecting;
                        }
                    }
                    Some(_) => continue,
                    None => continue,
                }
            }
            _ = delay => { continue; }
            res = &mut bg_task => {
                if let Err(e) =res {
                    bail!("bluetooth session background task exited with error: {e}");
                }
            }
            maybe_event = reader.next() => {
                if let Some(Ok(event)) = maybe_event {
                    match state {
                        State::Selecting => {
                            if let Event::Key(key) = event {
                                if key.kind == KeyEventKind::Press {
                                    match key.code {
                                        KeyCode::Char('q') => {
                                            let entries: Vec<MyEntry> = app.entries.into_values().collect();
                                            persist_history(&opt.persistent_file, &entries).await?;
                                            return Ok(())
                                        },
                                        KeyCode::Char('p') => {
                                            timeflip.pause().await?;
                                            state = State::Paused;
                                        }
                                        KeyCode::Char('d') => {
                                            if let Some(selected) = app.items.selected() {
                                                let entry = app.entries.get_mut(selected).expect("must be present");
                                                entry.visible = !entry.visible;
                                                if !entry.visible && !app.show_invisible {
                                                    app.items.remove();
                                                }
                                            }
                                        }
                                        KeyCode::Delete => {
                                            if let Some(selected) = app.items.selected() {
                                                let entry = app.entries.get_mut(selected).expect("must be present");
                                                if !entry.visible && app.show_invisible {
                                                    app.items.remove();
                                                    entry.deleted = true;
                                                }
                                            }
                                        }
                                        KeyCode::Char('b') => {
                                            let date_selection_list = StatefulList::with_items(app.get_available_dates(), None);
                                            state = State::Booking { date_selection_list, booking_date: None};
                                        }
                                        KeyCode::Char('t') => {
                                            app.toggle_visibility();
                                        }
                                        KeyCode::Char('s') => {
                                            let update: Vec<Entry> = timeflip
                                                .read_history_since(last_seen)
                                                .await?
                                                .into_iter()
                                                .collect();
                                            for entry in update {
                                                last_seen = max(entry.id, last_seen);
                                                match app.entries.entry(entry.id) {
                                                    Vacant(v) => {
                                                        v.insert(MyEntry {
                                                            entry,
                                                            description: vec![],
                                                            visible: true,
                                                            deleted: false,
                                                        });
                                                    }
                                                    Occupied(mut o) => {
                                                        o.get_mut().entry = entry;
                                                    }
                                                }
                                            }
                                            app.update_entry_list();
                                        }
                                        KeyCode::Right => {
                                            if app.items.selected().is_some() {
                                                state = State::Editing;
                                                textarea.set_style(Style::default().fg(Color::White));
                                            }
                                        }
                                        KeyCode::Down => {
                                            app.items.next();
                                        },
                                        KeyCode::Up => {
                                            app.items.previous();
                                        },
                                        _ => {}
                                    }
                                    let text = if let Some(selected) = app.items.selected() {
                                        app.entries.get(selected).expect("must be present").description.to_vec()
                                    } else { vec!["".to_string()] };
                                    textarea = TextArea::new(text);
                                }
                            }
                        },
                        State::Editing => {
                            match event.into() {
                                Input { key: Key::Esc, .. } => {
                                    state = State::Selecting;
                                    if let Some(editing_entry) = app.items.selected() {
                                        let entry = app.entries.get_mut(editing_entry).expect("must be present");
                                        entry.description = textarea.lines().to_vec();
                                    }
                                    textarea.set_style(Style::default().fg(Color::Gray));
                                },
                                input => {
                                    textarea.input(input);
                                }
                            }
                        }
                        State::Paused => {
                            match event.into() {
                                    Input { key: Key::Char('p'), .. } => {
                                        timeflip.unpause().await?;
                                        state = State::Selecting;
                                    }
                                    _ => {},
                            }
                        },
                        State::Booking{ref mut date_selection_list, ref mut booking_date} => {
                            if let Event::Key(key) = event {
                                if key.kind == KeyEventKind::Press {
                                    match key.code {
                                        KeyCode::Esc => {
                                            state = State::Selecting;
                                            if let Some(editing_entry) = app.items.selected() {
                                                let _entry = app.entries.get_mut(editing_entry).expect("must be present");
                                            }
                                        }
                                        KeyCode::Down => {
                                            date_selection_list.next();
                                        },
                                        KeyCode::Up => {
                                            date_selection_list.previous();
                                        }
                                        KeyCode::Enter => {
                                            terminal.draw(|f| show_loading_window(f))?;
                                            if let Some(date) = date_selection_list.selected() {
                                                let data = get_bookings_for_date(&app, &config, date).await;
                                                *booking_date = Some(data);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };
    }
}

fn show_main_window<B: Backend>(
    f: &mut Frame<B>,
    buf: Rect,
    app: &mut App,
    textarea: &TextArea,
    state: &State,
    config: &Config,
) {
    let inner_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(buf);
    let list_selected_color = match state {
        State::Selecting => Color::White,
        State::Editing | State::Paused => Color::Gray,
        _ => unreachable!("booking must not end up here"),
    };
    let max_len = longest_facet_name(config);
    let items: Vec<ListItem> = app
        .items
        .items
        .iter()
        .map(|i| {
            let entry = app.entries.get(i).expect("integrity broken");
            let spaced_facet = format!(
                "{:width$}",
                facet_name(&entry.entry.facet, config),
                width = max_len
            );
            let end_time = entry.entry.time
                + chrono::Duration::from_std(entry.entry.duration).expect("should work");
            let local = Local::now().timezone();
            let additional_info = if app.show_invisible {
                format!(" [{}]", if entry.visible { "*" } else { " " })
            } else {
                "".to_string()
            };
            let line_text = format!(
                "{} {}  {}  {}-{}{}",
                spaced_facet,
                entry.entry.time.with_timezone(&local).format("%d.%m"),
                DurationView(&entry.entry.duration),
                entry.entry.time.with_timezone(&local).format("%H:%M"),
                end_time.with_timezone(&local).format("%H:%M"),
                additional_info
            );
            let lines = Line::from(line_text);
            ListItem::new(lines).style(Style::default().fg(list_selected_color).bg(Color::Black))
        })
        .collect();
    let items = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Timeflip entries"),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(list_selected_color)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    f.render_stateful_widget(items, inner_layout[0], &mut app.items.state);
    f.render_widget(textarea.widget(), inner_layout[1]);
}

fn show_loading_window<B: Backend>(f: &mut Frame<B>) {
    let area = centered_rect(60, 25, f.size());
    let popup_block = Block::default()
        .title("Loading")
        .borders(Borders::NONE)
        .style(Style::default().bg(Color::DarkGray));
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    f.render_widget(popup_block, popup_layout[0]);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    // Cut the given rectangle into three vertical pieces
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    // Then cut the middle vertical piece into three width-wise pieces
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1] // Return the middle chunk
}

fn show_booking_window<B: Backend>(
    f: &mut Frame<B>,
    buf: Rect,
    app: &mut App,
    _textarea: &TextArea,
    state: &mut State,
    config: &Config,
) {
    let inner_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(buf);

    let (booking_date, date_selection_list) = if let State::Booking {
        date_selection_list,
        booking_date,
    } = state
    {
        (booking_date, date_selection_list)
    } else {
        unreachable!("only booking state here")
    };

    match booking_date {
        None => {
            let area = centered_rect(60, 25, f.size());
            let popup_block = Block::default()
                .title("Select a date to book:")
                .borders(Borders::NONE)
                .style(Style::default().bg(Color::DarkGray));
            let popup_layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(area);
            f.render_widget(popup_block, popup_layout[0]);
            let items: Vec<ListItem> = date_selection_list
                .items
                .iter()
                .map(|i| {
                    let lines = Line::from(format!("{}", i.format("%d.%m.%Y")));
                    ListItem::new(lines).style(Style::default().fg(Color::White).bg(Color::Black))
                })
                .collect();
            let items = List::new(items)
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol(">> ");
            f.render_stateful_widget(items, popup_layout[1], &mut date_selection_list.state);
        }
        Some(booking_date) => {
            f.render_widget(
                Paragraph::new(Line::from(format!(
                    "Date: {}",
                    booking_date.date.format("%d.%m.%Y")
                ))),
                inner_layout[0],
            );
            let max_len = booking_date
                .suggested_bookings
                .iter()
                .map(|(booking_type, _)| match booking_type {
                    BookingType::Jira(issue) => issue.len(),
                    BookingType::Unknown(facet) => facet_name(&facet, config).len(),
                })
                .max()
                .unwrap_or_default();
            let mut booked_time = Duration::new(0, 0);
            let accumulated_text = booking_date
                .suggested_bookings
                .iter()
                .map(|(booking_type, booking)| match booking_type {
                    BookingType::Jira(issue) => {
                        let additional_jira_data =
                            if let Some(fields) = booking_date.jira_data.get(issue) {
                                format!(" {}", fields.summary)
                            } else {
                                "".to_string()
                            };
                        booked_time += booking.duration;
                        format!(
                            "{:width$} {} {}\n",
                            issue.to_string(),
                            DurationView(&booking.duration),
                            additional_jira_data,
                            width = max_len
                        )
                    }
                    BookingType::Unknown(facet) => {
                        let additional_descriptions: Vec<_> = booking
                            .based_on_entries
                            .iter()
                            .filter_map(|id| {
                                let description = app
                                    .entries
                                    .get(id)
                                    .expect("inconsistent data")
                                    .description
                                    .join("|");
                                if !description.is_empty() {
                                  Some(format!("<{}>", description))
                                } else {
                                  None
                                }
                            })
                            .collect();
                        format!(
                            "{:width$} {} ({})\n",
                            facet_name(&facet, config),
                            DurationView(&booking.duration),
                            additional_descriptions.join(" "),
                            width = max_len
                        )
                    }
                })
                .collect::<String>();
            f.render_widget(
                Paragraph::new(format!(
                    "Total duration: {} ({} booked)",
                    DurationView(&booking_date.total_duration),
                    DurationView(&booked_time)
                )),
                inner_layout[1],
            );
            f.render_widget(Paragraph::new(accumulated_text), inner_layout[3]);
        }
    }
}

fn ui<B: Backend>(
    f: &mut Frame<B>,
    app: &mut App,
    textarea: &TextArea,
    state: &mut State,
    config: &Config,
) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.size());
    let area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Min(0), Constraint::Length(45)])
        .split(main_layout[0]);

    let mid_gray = Color::Indexed(244);
    let white = Color::Indexed(255);
    f.render_widget(
        Paragraph::new(Span::styled(
            "Timekeeper",
            Style::new().fg(white).add_modifier(Modifier::BOLD),
        )),
        area[0],
    );

    let titles = vec![" TimeFlip2 ", " Jira "];
    let selected = match state {
        State::Selecting | State::Editing | State::Paused => {
            show_main_window(f, main_layout[2], app, textarea, state, config);
            0
        }
        State::Booking { .. } => {
            show_booking_window(f, main_layout[2], app, textarea, state, config);
            1
        }
    };
    f.render_widget(
        ratatui::widgets::Tabs::new(titles)
            .style(Style::new().fg(mid_gray))
            .highlight_style(
                Style::new()
                    .fg(white)
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::REVERSED),
            )
            .select(selected)
            .divider(""),
        area[1],
    );
    f.render_widget(
        Block::new()
            .borders(Borders::TOP)
            .title(state.get_description()),
        main_layout[3],
    );
}
