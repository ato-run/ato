-- Add capsules table and extend deployments schema for capsule runs

-- Capsules represent the long-lived manifest metadata
CREATE TABLE IF NOT EXISTS capsules (
    capsule_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    manifest_json TEXT NOT NULL,
    annotations_json TEXT,
    metadata_json TEXT,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Ensure indexes to speed up lookups on capsule id and manifest name
CREATE INDEX IF NOT EXISTS idx_capsules_name ON capsules(name);

-- Extend deployments table to carry richer run metadata
ALTER TABLE deployments ADD COLUMN metadata_json TEXT;
ALTER TABLE deployments ADD COLUMN progress_json TEXT;
ALTER TABLE deployments ADD COLUMN error_json TEXT;
ALTER TABLE deployments ADD COLUMN updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP;

-- Provide an index on capsule_id for fast filtering
CREATE INDEX IF NOT EXISTS idx_deployments_capsule_id ON deployments(capsule_id);

-- Existing status index may not exist in older databases; recreate under defensive guard
DROP INDEX IF EXISTS idx_deployments_status;
CREATE INDEX idx_deployments_status ON deployments(status);
