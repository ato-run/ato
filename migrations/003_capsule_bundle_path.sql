-- Add bundle_path column to capsules for storing canonical bundle archives
ALTER TABLE capsules ADD COLUMN bundle_path TEXT;
