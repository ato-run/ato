use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedSender;

use crate::search;

use super::event::AppEvent;

#[derive(Clone)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub registry: Option<String>,
}

pub fn spawn_search(handle: &Handle, tx: UnboundedSender<AppEvent>, seq: u64, req: SearchRequest) {
    let tx_done = tx.clone();
    handle.spawn(async move {
        let result = search::search_capsules(
            req.query.as_deref(),
            req.category.as_deref(),
            Some(req.tags.as_slice()),
            req.limit,
            req.cursor.as_deref(),
            req.registry.as_deref(),
        )
        .await
        .map_err(|e| e.to_string());

        let _ = tx_done.send(AppEvent::SearchCompleted { seq, result });
    });
}

pub fn spawn_manifest_fetch(
    handle: &Handle,
    tx: UnboundedSender<AppEvent>,
    scoped_id: String,
    registry: Option<String>,
) {
    let tx_done = tx.clone();
    handle.spawn(async move {
        let result = search::fetch_capsule_manifest(&scoped_id, registry.as_deref())
            .await
            .map_err(|e| e.to_string());
        let _ = tx_done.send(AppEvent::ManifestCompleted { scoped_id, result });
    });
}
