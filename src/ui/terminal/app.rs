use std::time::Instant;
use std::{collections::HashMap, collections::HashSet};

use tui_input::Input;

use crate::commands::search::CapsuleSummary;

pub struct App {
    pub query: Input,
    pub input_mode: bool,
    pub items: Vec<CapsuleSummary>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub hint: Option<String>,
    pub should_quit: bool,
    pub accepted_scoped_id: Option<String>,
    pub has_user_interaction: bool,
    pub query_dirty: bool,
    pub last_query_change_at: Instant,
    pub active_seq: u64,
    pub show_manifest: bool,
    pub manifest_cache: HashMap<String, String>,
    pub manifest_errors: HashMap<String, String>,
    pub manifest_inflight: HashSet<String>,
}

impl App {
    pub fn new(initial_query: Option<&str>, show_manifest: bool) -> Self {
        let mut query = Input::default();
        if let Some(initial) = initial_query {
            query = Input::new(initial.to_string());
        }

        Self {
            query,
            input_mode: false,
            items: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: false,
            error: None,
            hint: None,
            should_quit: false,
            accepted_scoped_id: None,
            has_user_interaction: false,
            query_dirty: true,
            last_query_change_at: Instant::now(),
            active_seq: 0,
            show_manifest,
            manifest_cache: HashMap::new(),
            manifest_errors: HashMap::new(),
            manifest_inflight: HashSet::new(),
        }
    }

    pub fn selected_item(&self) -> Option<&CapsuleSummary> {
        self.items.get(self.selected)
    }

    pub fn move_down(&mut self) {
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
    }

    pub fn move_up(&mut self) {
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn mark_query_changed(&mut self) {
        self.query_dirty = true;
        self.last_query_change_at = Instant::now();
        self.error = None;
    }

    pub fn current_query(&self) -> String {
        self.query.value().trim().to_string()
    }

    pub fn selected_scoped_id(&self) -> Option<String> {
        self.selected_item().map(|item| {
            item.scoped_id
                .clone()
                .unwrap_or_else(|| format!("{}/{}", item.publisher.handle, item.slug))
        })
    }

    pub fn clear_manifest_state(&mut self) {
        self.manifest_cache.clear();
        self.manifest_errors.clear();
        self.manifest_inflight.clear();
    }

    pub fn mark_user_interaction(&mut self) {
        self.has_user_interaction = true;
    }

    pub fn can_accept_enter_selection(&self) -> bool {
        self.has_user_interaction && !self.loading && !self.items.is_empty()
    }
}
