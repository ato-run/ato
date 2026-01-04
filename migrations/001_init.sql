-- Port allocations
CREATE TABLE port_allocations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    port INTEGER NOT NULL UNIQUE,
    operation_id TEXT NOT NULL,
    allocated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    released_at DATETIME,
    UNIQUE(operation_id, port)
);

CREATE INDEX idx_port_allocations_port ON port_allocations(port);
CREATE INDEX idx_port_allocations_released ON port_allocations(released_at);

-- Deployments
CREATE TABLE deployments (
    id TEXT PRIMARY KEY,  -- UUID
    capsule_id TEXT NOT NULL,
    status TEXT NOT NULL,  -- pending, uploading, extracting, starting, running, failed, stopped
    port INTEGER,
    public_domain TEXT,
    container_id TEXT,
    error_message TEXT,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    started_at DATETIME,
    completed_at DATETIME
);

CREATE INDEX idx_deployments_status ON deployments(status);

-- Deployment metrics
CREATE TABLE deployment_metrics (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    operation_id TEXT NOT NULL,
    total_ms INTEGER NOT NULL,
    upload_ms INTEGER,
    extract_ms INTEGER,
    container_ms INTEGER,
    caddy_ms INTEGER,
    recorded_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (operation_id) REFERENCES deployments(id)
);

CREATE INDEX idx_deployment_metrics_operation ON deployment_metrics(operation_id);
