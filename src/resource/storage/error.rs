use thiserror::Error;

/// Storage operation errors
#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Command execution failed: {0}")]
    CommandFailed(String),

    #[error("Volume not found: {0}")]
    VolumeNotFound(String),

    #[error("Volume already exists: {0}")]
    VolumeAlreadyExists(String),

    #[error("Invalid volume name: {0}")]
    InvalidVolumeName(String),

    #[error("Invalid size specification: {0}")]
    InvalidSize(String),

    #[error("Encryption error: {0}")]
    EncryptionError(String),

    #[error("Key management error: {0}")]
    KeyManagementError(String),

    #[error("Insufficient space: required {required}, available {available}")]
    InsufficientSpace { required: u64, available: u64 },

    #[error("Snapshot error: {0}")]
    SnapshotError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Volume is busy: {0}")]
    VolumeBusy(String),
}

/// Result type for storage operations
pub type StorageResult<T> = Result<T, StorageError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_error_display() {
        let err = StorageError::VolumeNotFound("test_vol".to_string());
        assert_eq!(err.to_string(), "Volume not found: test_vol");

        let err = StorageError::VolumeAlreadyExists("existing_vol".to_string());
        assert_eq!(err.to_string(), "Volume already exists: existing_vol");

        let err = StorageError::InvalidVolumeName("bad@name".to_string());
        assert_eq!(err.to_string(), "Invalid volume name: bad@name");

        let err = StorageError::InsufficientSpace {
            required: 1000,
            available: 500,
        };
        assert_eq!(
            err.to_string(),
            "Insufficient space: required 1000, available 500"
        );
    }

    #[test]
    fn test_command_failed_error() {
        let err = StorageError::CommandFailed("lvcreate failed".to_string());
        assert!(err.to_string().contains("Command execution failed"));
    }

    #[test]
    fn test_encryption_errors() {
        let err = StorageError::EncryptionError("LUKS format failed".to_string());
        assert!(err.to_string().contains("Encryption error"));

        let err = StorageError::KeyManagementError("Key not found".to_string());
        assert!(err.to_string().contains("Key management error"));
    }

    #[test]
    fn test_snapshot_error() {
        let err = StorageError::SnapshotError("COW allocation failed".to_string());
        assert!(err.to_string().contains("Snapshot error"));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let storage_err: StorageError = io_err.into();
        assert!(storage_err.to_string().contains("IO error"));
        assert!(storage_err.to_string().contains("file not found"));
    }

    #[test]
    fn test_parse_error() {
        let err = StorageError::ParseError("Invalid size format".to_string());
        assert!(err.to_string().contains("Parse error"));
    }

    #[test]
    fn test_permission_denied() {
        let err = StorageError::PermissionDenied("Root required".to_string());
        assert!(err.to_string().contains("Permission denied"));
    }

    #[test]
    fn test_volume_busy() {
        let err = StorageError::VolumeBusy("/dev/vg/lv".to_string());
        assert!(err.to_string().contains("Volume is busy"));
    }

    #[test]
    fn test_result_type() {
        fn returns_ok() -> StorageResult<String> {
            Ok("success".to_string())
        }

        fn returns_err() -> StorageResult<String> {
            Err(StorageError::VolumeNotFound("test".to_string()))
        }

        assert!(returns_ok().is_ok());
        assert!(returns_err().is_err());
    }
}
