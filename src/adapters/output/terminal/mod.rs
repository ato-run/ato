use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self as crossterm_event, Event};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

mod app;
pub mod common;
mod event;
mod handler;
mod network;
mod ui;
mod widgets;

use self::app::App;
use self::event::AppEvent;
use self::network::SearchRequest;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
const STARTUP_INPUT_DRAIN_WINDOW: Duration = Duration::from_millis(120);
const STARTUP_ENTER_GUARD_WINDOW: Duration = Duration::from_millis(200);

pub use self::common::can_launch_tui;

pub struct SearchTuiArgs {
    pub query: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub registry: Option<String>,
    pub show_manifest: bool,
}

pub fn run_search_tui(args: SearchTuiArgs) -> Result<Option<String>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    common::drain_startup_input_events(STARTUP_INPUT_DRAIN_WINDOW)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let tui_started_at = Instant::now();

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(args.query.as_deref(), args.show_manifest);

    let request_base = SearchRequest {
        query: args.query,
        category: args.category,
        tags: args.tags,
        limit: args.limit,
        cursor: args.cursor,
        registry: args.registry,
    };

    let mut next_seq = 1u64;
    let trigger_search = |app: &mut App,
                          tx: &mpsc::UnboundedSender<AppEvent>,
                          handle: &tokio::runtime::Handle,
                          request_base: &SearchRequest,
                          next_seq: &mut u64| {
        let mut req = request_base.clone();
        let q = app.current_query();
        req.query = if q.is_empty() { None } else { Some(q) };

        app.active_seq = *next_seq;
        app.loading = true;
        app.query_dirty = false;

        network::spawn_search(handle, tx.clone(), *next_seq, req);
        *next_seq += 1;
    };

    trigger_search(&mut app, &tx, rt.handle(), &request_base, &mut next_seq);

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        while let Ok(msg) = rx.try_recv() {
            match msg {
                AppEvent::SearchCompleted { seq, result } => {
                    if seq != app.active_seq {
                        continue;
                    }
                    app.loading = false;
                    match result {
                        Ok(res) => {
                            app.items = res.capsules;
                            app.next_cursor = res.next_cursor;
                            app.selected = app.selected.min(app.items.len().saturating_sub(1));
                            app.error = None;
                            app.clear_manifest_state();
                        }
                        Err(err) => {
                            app.error = Some(err);
                            app.items.clear();
                            app.next_cursor = None;
                            app.selected = 0;
                        }
                    }
                }
                AppEvent::ManifestCompleted { scoped_id, result } => {
                    app.manifest_inflight.remove(&scoped_id);
                    match result {
                        Ok(content) => {
                            app.manifest_errors.remove(&scoped_id);
                            app.manifest_cache.insert(scoped_id, content);
                        }
                        Err(err) => {
                            app.manifest_cache.remove(&scoped_id);
                            app.manifest_errors.insert(scoped_id, err);
                        }
                    }
                }
                AppEvent::Input(key) => {
                    handler::handle_key_event(&mut app, key);
                }
                AppEvent::Tick => {}
            }
        }

        if app.show_manifest {
            if let Some(scoped_id) = app.selected_scoped_id() {
                let already_loaded = app.manifest_cache.contains_key(&scoped_id)
                    || app.manifest_errors.contains_key(&scoped_id);
                if !already_loaded && !app.manifest_inflight.contains(&scoped_id) {
                    app.manifest_inflight.insert(scoped_id.clone());
                    network::spawn_manifest_fetch(
                        rt.handle(),
                        tx.clone(),
                        scoped_id,
                        request_base.registry.clone(),
                    );
                }
            }
        }

        if app.query_dirty && app.last_query_change_at.elapsed() >= SEARCH_DEBOUNCE && !app.loading
        {
            trigger_search(&mut app, &tx, rt.handle(), &request_base, &mut next_seq);
        }

        if app.should_quit {
            break;
        }

        if crossterm_event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = crossterm_event::read()? {
                if common::should_ignore_startup_enter(
                    &key,
                    tui_started_at,
                    STARTUP_ENTER_GUARD_WINDOW,
                ) {
                    continue;
                }
                let _ = tx.send(AppEvent::Input(key));
            }
        } else {
            let _ = tx.send(AppEvent::Tick);
        }
    }

    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    Ok(app.accepted_scoped_id)
}
