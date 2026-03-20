use super::*;

pub(super) fn build_app_router(ui_enabled: bool) -> Router<AppState> {
    let mut app = Router::new();
    app = app.route("/.well-known/capsule.json", get(handle_well_known));
    app = app.route("/v1/capsules", get(handle_search_capsules));
    app = app.route("/v1/manifest/capsules", get(handle_search_capsules));
    app = app.route(
        "/v1/manifest/capsules/by/:publisher/:slug",
        get(handle_get_capsule),
    );
    app = app.route("/v1/capsules/by/:publisher/:slug", get(handle_get_capsule));
    app = app.route(
        "/v1/manifest/capsules/by/:publisher/:slug/distributions",
        get(handle_distributions),
    );
    app = app.route(
        "/v1/capsules/by/:publisher/:slug/distributions",
        get(handle_distributions),
    );
    app = app.route(
        "/v1/manifest/capsules/by/:publisher/:slug/download",
        get(handle_download),
    );
    app = app.route(
        "/v1/capsules/by/:publisher/:slug/download",
        get(handle_download),
    );
    app = app.route("/v1/manifest/negotiate", post(handle_manifest_negotiate));
    app = app.route(
        "/v1/manifest/resolve/:publisher/:slug/:version",
        get(handle_manifest_resolve_version),
    );
    app = app.route(
        "/v1/manifest/documents/:manifest_hash",
        get(handle_manifest_get_manifest),
    );
    app = app.route(
        "/v1/manifest/chunks/:chunk_hash",
        get(handle_manifest_get_chunk),
    );
    app = app.route(
        "/v1/manifest/epoch/resolve",
        post(handle_manifest_epoch_resolve),
    );
    app = app.route(
        "/v1/manifest/leases/refresh",
        post(handle_manifest_lease_refresh),
    );
    app = app.route(
        "/v1/manifest/leases/release",
        post(handle_manifest_lease_release),
    );
    app = app.route("/v1/manifest/keys/rotate", post(handle_manifest_key_rotate));
    app = app.route("/v1/manifest/keys/revoke", post(handle_manifest_key_revoke));
    app = app.route("/v1/manifest/rollback", post(handle_manifest_rollback));
    app = app.route("/v1/manifest/yank", post(handle_manifest_yank));
    app = app.route(
        "/v1/artifacts/:publisher/:slug/:version/:file_name",
        get(handle_get_artifact),
    );
    app = app.route("/v1/sync/negotiate", post(handle_sync_negotiate));
    app = app.route("/v1/sync/commit", post(handle_sync_commit));
    app = app.route("/v1/chunks/:raw_hash", put(handle_put_chunk));
    app = app.route("/v1/chunks/:raw_hash", get(handle_get_chunk));
    app = app.route(
        "/v1/releases/:publisher/:slug/:version/manifest",
        get(handle_get_release_manifest),
    );
    app = app.route(
        "/v1/local/capsules/:publisher/:slug/:version",
        put(handle_put_local_capsule),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug/store-metadata",
        get(handle_get_store_metadata).put(handle_put_store_metadata),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug/runtime-config",
        get(handle_get_runtime_config).put(handle_put_runtime_config),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug/store-icon",
        get(handle_get_store_icon),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug",
        delete(handle_delete_local_capsule),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug/run",
        post(handle_run_local_capsule),
    );
    app = app.route(
        "/v1/local/states",
        get(handle_list_persistent_states).post(handle_register_persistent_state),
    );
    app = app.route(
        "/v1/local/states/:state_id",
        get(handle_get_persistent_state),
    );
    app = app.route(
        "/v1/local/bindings",
        get(handle_list_service_bindings).post(handle_register_service_binding),
    );
    app = app.route(
        "/v1/local/bindings/resolve",
        get(handle_resolve_service_binding),
    );
    app = app.route(
        "/v1/local/bindings/:binding_id",
        get(handle_get_service_binding),
    );
    app = app.route("/v1/local/processes", get(handle_list_local_processes));
    app = app.route("/v1/local/url-ready", get(handle_local_url_ready));
    app = app.route(
        "/v1/local/processes/:id/stop",
        post(handle_stop_local_process),
    );
    app = app.route(
        "/v1/local/processes/:id/logs",
        get(handle_get_process_logs).delete(handle_clear_process_logs),
    );

    if ui_enabled {
        app = app.fallback(handle_ui_request);
    }

    app
}
