use std::env;
use std::path::{Path, PathBuf};

pub const TARGET_EXE: &str = "Condor.exe";
pub const IFEO_PATH: &str = r#"Software\Microsoft\Windows NT\CurrentVersion\Image File Execution Options"#;
pub const SETTINGS_PATH: &str = r#"Software\CondorVR"#;
pub const SERVICE_NAME: &str = "CondorReviveHelperService";
pub const LAUNCHER_EXE_NAME: &str = "CondorVR.exe";
pub const CONFIGURER_EXE_NAME: &str = "Condor-VR-Configurer.exe";

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

/// Gets the path to a companion executable in the same directory as the current process.
pub fn get_companion_exe_path(exe_name: &str) -> Option<PathBuf> {
    let mut path = env::current_exe().ok()?;
    path.pop();
    path.push(exe_name);
    Some(path)
}

/// Handles the --version or -v command line arguments.
/// Returns true if the version was printed and the program should exit.
pub fn handle_version_args(program_name: &str) -> bool {
    let args: Vec<String> = env::args().collect();
    if args.contains(&"--version".to_string()) || args.contains(&"-v".to_string()) {
        println!("{} version {}", program_name, env!("CARGO_PKG_VERSION"));
        true
    } else {
        false
    }
}

/// Finds the ReviveInjector executable path from registry or common locations.
pub fn find_revive_injector() -> Option<String> {
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::*;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        if let Ok(settings_key) = hklm.open_subkey(SETTINGS_PATH) {
            if let Ok(revive_path) = settings_key.get_value::<String, _>("ReviveInjectorPath") {
                if Path::new(&revive_path).exists() {
                    return Some(revive_path);
                }
            }
        }
    }

    let fallbacks = [
        r#"C:\Program Files\Revive\Revive\ReviveInjector.exe"#,
        r#"C:\Program Files\Revive\Revive\x64\ReviveInjector.exe"#,
        r#"C:\Program Files\Revive\ReviveInjector.exe"#,
    ];

    for fallback in fallbacks {
        if Path::new(fallback).exists() {
            return Some(fallback.to_string());
        }
    }
    None
}

/// Checks if the IFEO hook for Condor.exe is present in the registry.
pub fn is_ifeo_hook_present() -> bool {
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::*;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let target_key_path = format!(r#"{}\{}"#, IFEO_PATH, TARGET_EXE);
        if let Ok(key) = hklm.open_subkey(target_key_path) {
            return key.get_value::<String, _>("Debugger").is_ok();
        }
    }
    false
}
