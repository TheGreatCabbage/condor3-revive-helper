#![windows_subsystem = "windows"]

use std::env;
use std::io;
use winreg::enums::*;
use winreg::RegKey;

const TARGET_EXE: &str = "Condor.exe";
const IFEO_PATH: &str = r#"Software\Microsoft\Windows NT\CurrentVersion\Image File Execution Options"#;

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: Condor-VR-Configurer.exe [activate|deactivate]");
        return Ok(());
    }

    let command = &args[1];
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let target_key_path = format!(r#"{}\{}"#, IFEO_PATH, TARGET_EXE);

    match command.as_str() {
        "activate" => {
            let mut launcher_path = env::current_exe()?;
            launcher_path.pop();
            launcher_path.push("CondorVR.exe");
            let launcher_path_str = launcher_path.to_str().expect("Invalid path");

            let (target_key, _) = hklm.create_subkey_with_flags(&target_key_path, KEY_ALL_ACCESS)?;
            let launcher_command = format!("\"{}\"", launcher_path_str);
            target_key.set_value("Debugger", &launcher_command)?;
            println!("Hook activated with: {}", launcher_command);
        }
        "deactivate" => {
            if let Ok(key) = hklm.open_subkey_with_flags(&target_key_path, KEY_ALL_ACCESS) {
                let _ = key.delete_value("Debugger");
                println!("Hook deactivated.");
            }
        }
        _ => println!("Unknown command: {}", command),
    }

    Ok(())
}
