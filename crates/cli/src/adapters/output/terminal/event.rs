use crossterm::event::KeyEvent;

use crate::search::SearchResult;

pub enum AppEvent {
    Input(KeyEvent),
    Tick,
    SearchCompleted {
        seq: u64,
        result: Result<SearchResult, String>,
    },
    ManifestCompleted {
        scoped_id: String,
        result: Result<String, String>,
    },
}
