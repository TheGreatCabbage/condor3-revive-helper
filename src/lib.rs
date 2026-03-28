use std::env;
use std::path::{Path, PathBuf};

/// Validates that a path is not a symbolic link or junction (reparse point).
pub fn is_safe_path(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            // Check for FILE_ATTRIBUTE_REPARSE_POINT (0x400)
            if (metadata.file_attributes() & 0x400) != 0 {
                return false;
            }
            return true;
        }
    }
    true
}

/// Gets a secure path for log files in ProgramData, falling back to Temp if reparse points are detected.
pub fn get_secure_log_path(subdir: &str, filename: &str) -> PathBuf {
    let mut path = if let Some(pd) = env::var_os("ProgramData") {
        PathBuf::from(pd)
    } else {
        PathBuf::from(r"C:\ProgramData")
    };
    path.push(subdir);

    if path.exists() {
        if !is_safe_path(&path) {
            return env::temp_dir().join(format!("{}_{}", subdir, filename));
        }
    } else {
        // If it doesn't exist, try to create it.
        let _ = std::fs::create_dir_all(&path);
        // Re-check after creation to avoid TOCTOU (or at least detect it).
        if path.exists() && !is_safe_path(&path) {
            return env::temp_dir().join(format!("{}_{}", subdir, filename));
        }
    }

    path.push(filename);
    
    if path.exists() && !is_safe_path(&path) {
        return env::temp_dir().join(format!("{}_{}", subdir, filename));
    }

    path
}
