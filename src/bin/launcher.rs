//! The launcher which is triggered by the registry key when Condor.exe is executed;
//! it then launches Condor with ReviveInjector, setting flags to avoid the registry
//! hook triggering again.

#![windows_subsystem = "windows"]

use std::env;
#[cfg(feature = "logging")]
use std::fs::OpenOptions;
#[cfg(feature = "logging")]
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::{thread, time::Duration, time::Instant};

use windows::Win32::Foundation::*;
use windows::Win32::System::Services::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::core::PCWSTR;

use eframe::egui;

use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::{DACL_SECURITY_INFORMATION, ACCESS_MASK, ACCESS_ALLOWED_ACE, ACL, ACE_HEADER};

fn read_env_var_from_file(var_name: &str) -> Option<String> {
    if let Ok(mut exe_path) = env::current_exe() {
        exe_path.pop();
        exe_path.push(".env");
        if exe_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&exe_path) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.starts_with('#') || line.is_empty() {
                        continue;
                    }
                    if let Some((key, value)) = line.split_once('=') {
                        if key.trim() == var_name {
                            let val = value.trim();
                            // Remove optional quotes
                            if (val.starts_with('"') && val.ends_with('"')) || (val.starts_with('\'') && val.ends_with('\'')) {
                                return Some(val[1..val.len()-1].to_string());
                            }
                            return Some(val.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Checks if a file has strict permissions (not writable by non-admins/standard users).
fn has_strict_permissions(path: &Path) -> bool {
    unsafe {
        let path_w: Vec<u16> = path.to_str().unwrap_or("").encode_utf16().chain(Some(0)).collect();
        let mut p_psid_owner = std::ptr::null_mut();
        let mut p_psid_group = std::ptr::null_mut();
        let mut p_dacl = std::ptr::null_mut();
        let mut p_security_descriptor = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();

        let res = GetNamedSecurityInfoW(
            PCWSTR(path_w.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            Some(&mut p_psid_owner),
            Some(&mut p_psid_group),
            Some(&mut p_dacl),
            None,
            &mut p_security_descriptor,
        );

        if res != ERROR_SUCCESS {
            return false;
        }

        if p_dacl.is_null() {
            let _ = LocalFree(Some(HLOCAL(p_security_descriptor.0)));
            return false; // A NULL DACL means Everyone has full access, so not strict.
        }

        // Check the DACL for entries that grant write access to non-privileged groups
        let mut acl_size_info = windows::Win32::Security::ACL_SIZE_INFORMATION::default();
        if !windows::Win32::Security::GetAclInformation(
            p_dacl,
            &mut acl_size_info as *mut _ as *mut _,
            std::mem::size_of::<windows::Win32::Security::ACL_SIZE_INFORMATION>() as u32,
            windows::Win32::Security::AclSizeInformation,
        ).is_ok() {
            let _ = LocalFree(Some(HLOCAL(p_security_descriptor.0)));
            return false;
        }

        for i in 0..acl_size_info.AceCount {
            let mut p_ace = std::ptr::null_mut();
            if windows::Win32::Security::GetAce(p_dacl, i, &mut p_ace).is_ok() {
                let header = &*(p_ace as *const ACE_HEADER);
                // We only care about Access Allowed ACEs for this check
                if header.AceType == windows::Win32::Security::ACCESS_ALLOWED_ACE_TYPE {
                    let ace = &*(p_ace as *const ACCESS_ALLOWED_ACE);
                    let mask = ace.Mask;
                    
                    // Check if this ACE grants write permissions
                    let write_mask = 0x00000002 | 0x00000004 | 0x00010000 | 0x00100000; // FILE_WRITE_DATA | FILE_APPEND_DATA | DELETE | GENERIC_WRITE
                    if (mask & write_mask) != 0 {
                        let sid = &ace.SidStart as *const _ as *const windows::Win32::Security::SID;
                        
                        // Check if the SID is a non-privileged group (like Everyone, Users, Authenticated Users)
                        // S-1-1-0 (Everyone)
                        // S-1-5-11 (Authenticated Users)
                        // S-1-5-32-545 (Users)
                        let mut sid_string = windows::core::PWSTR::null();
                        if windows::Win32::Security::ConvertSidToStringSidW(sid, &mut sid_string).is_ok() {
                            let s = String::from_utf16_lossy(sid_string.as_wide());
                            let _ = LocalFree(Some(HLOCAL(sid_string.0 as *mut _)));
                            
                            if s == "S-1-1-0" || s == "S-1-5-11" || s == "S-1-5-32-545" {
                                // Found a non-privileged SID with write access
                                let _ = LocalFree(Some(HLOCAL(p_security_descriptor.0)));
                                return false;
                            }
                        }
                    }
                }
            }
        }

        let _ = LocalFree(Some(HLOCAL(p_security_descriptor.0)));
    }
    true
}

const BYPASS_SERVICE_NAME: &str = "CondorReviveHelperService";
const SETTINGS_PATH: &str = r#"Software\CondorVR"#;

fn trigger_bypass_service() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let scm = match OpenSCManagerW(None, None, SC_MANAGER_CONNECT) {
            Ok(h) => h,
            Err(e) => {
                log(&format!("Error: OpenSCManagerW failed: {}", e));
                return Err(e.into());
            }
        };
        
        let service_name_w: Vec<u16> = BYPASS_SERVICE_NAME.encode_utf16().chain(Some(0)).collect();
        let service = match OpenServiceW(
            scm,
            PCWSTR(service_name_w.as_ptr()),
            SERVICE_START | SERVICE_QUERY_STATUS,
        ) {
            Ok(h) => h,
            Err(e) => {
                log(&format!("Error: OpenServiceW failed for {}: {}", BYPASS_SERVICE_NAME, e));
                let _ = CloseServiceHandle(scm);
                return Err(e.into());
            }
        };

        if StartServiceW(service, None).is_ok() {
            log("Service start signal sent successfully.");
        } else {
            let err = windows::core::Error::from_thread();
            if err.code() == ERROR_SERVICE_ALREADY_RUNNING.to_hresult() {
                log("Service is already running.");
            } else {
                log(&format!("Error: StartServiceW failed: {}", err));
                let _ = CloseServiceHandle(service);
                let _ = CloseServiceHandle(scm);
                return Err(err.into());
            }
        }

        // Wait for the service to be in RUNNING state
        let mut status = SERVICE_STATUS::default();
        let start_wait = Instant::now();
        let timeout = Duration::from_secs(5);

        while start_wait.elapsed() < timeout {
            if QueryServiceStatus(service, &mut status).is_ok() {
                if status.dwCurrentState == SERVICE_RUNNING {
                    log("Service is now running.");
                    break;
                } else if status.dwCurrentState == SERVICE_STOPPED {
                    log("Service stopped unexpectedly.");
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        // Now explicitly wait for the IFEO hook (registry key) to be deleted
        let hook_wait = Instant::now();
        while hook_wait.elapsed() < timeout {
            if !is_ifeo_hook_present() {
                log("IFEO hook confirmed deleted.");
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        if is_ifeo_hook_present() {
            log("Error: IFEO hook still present after timeout.");
            let _ = CloseServiceHandle(service);
            let _ = CloseServiceHandle(scm);
            return Err("IFEO hook could not be removed. Please ensure the CondorReviveHelperService is running or restart your computer.".into());
        }

        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);
        Ok(())
    }
}

fn is_ifeo_hook_present() -> bool {
    unsafe {
        let hklm = HKEY_LOCAL_MACHINE;
        let subkey_path = r"Software\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Condor.exe";
        let subkey_wide: Vec<u16> = subkey_path.encode_utf16().chain(Some(0)).collect();

        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            hklm,
            PCWSTR(subkey_wide.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        ) != ERROR_SUCCESS {
            return false;
        }

        let value_name: Vec<u16> = "Debugger".encode_utf16().chain(Some(0)).collect();
        let res = RegQueryValueExW(
            hkey,
            PCWSTR(value_name.as_ptr()),
            None,
            None,
            None,
            None,
        );

        let _ = RegCloseKey(hkey);
        res == ERROR_SUCCESS
    }
}

fn log(msg: &str) {
    println!("{}", msg);

    #[cfg(feature = "logging")]
    {
        let mut log_path = if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            let mut p = std::path::PathBuf::from(local_app_data);
            p.push("CondorVR");
            let _ = std::fs::create_dir_all(&p);
            p
        } else {
            return;
        };
        log_path.push("CondorVR_log.txt");

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
        {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            let line = format!("[{}] {}\n", timestamp, msg);
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

struct LauncherState {
    progress: AtomicU32,
    finished: AtomicBool,
    error_message: std::sync::Mutex<Option<String>>,
}

struct LauncherApp {
    state: Arc<LauncherState>,
    is_manual: bool,
}

impl LauncherApp {
    fn new(state: Arc<LauncherState>, is_manual: bool) -> Self {
        Self { state, is_manual }
    }

    fn open_settings(&self) {
        if let Ok(mut p) = env::current_exe() {
            p.pop();
            p.push("gui.exe");

            if p.exists() {
                let mut cmd = std::process::Command::new(p);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
                }
                let _ = cmd.spawn();
            }
        }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let has_error = self.state.error_message.lock().unwrap().is_some();
        if !self.is_manual && self.state.finished.load(Ordering::Relaxed) && !has_error {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(msg) = self.state.error_message.lock().unwrap().clone() {
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    ui.heading("Launch Error");
                    ui.add_space(10.0);
                    ui.label(&msg);
                    ui.add_space(20.0);
                    if ui.button("Close").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                return;
            }

            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                if self.is_manual {
                    ui.heading("Condor VR Helper");
                    ui.add_space(10.0);
                    ui.label("Whenever Condor is launched, it will open via this program");
                    ui.label("as long as VR is enabled in the Condor3 Revive Helper.");
                    ui.add_space(20.0);
                } else {
                    ui.heading("Starting Condor with VR...");
                    ui.add_space(15.0);

                    let progress_bits = self.state.progress.load(Ordering::Relaxed);
                    let progress = f32::from_bits(progress_bits);
                    ui.add(egui::ProgressBar::new(progress).show_percentage());

                    ui.add_space(15.0);
                    ui.label("Initializing VR support...");
                    ui.label("This window will close automatically.");
                    ui.add_space(10.0);
                }

                ui.add_space(5.0);
                if ui
                    .add_sized([140.0, 32.0], egui::Button::new("Open VR Settings"))
                    .clicked()
                {
                    self.open_settings();
                }

                if self.is_manual {
                    ui.add_space(15.0);
                    if ui
                        .add_sized([140.0, 32.0], egui::Button::new("Exit"))
                        .clicked()
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    ui.add_space(20.0);
                } else {
                    ui.add_space(10.0);
                }
            });
        });

        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = env::args().collect();
    if args.contains(&"--version".to_string()) || args.contains(&"-v".to_string()) {
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
        println!("CondorVR version {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    log(&format!("Launcher started with args: {:?}", args));

    let is_manual = args.len() < 2;

    // Parse target path and game args
    let mut target_parts = Vec::new();
    let mut game_args = Vec::new();
    let mut found_args = false;

    if !is_manual {
        for arg in args.iter().skip(1) {
            if found_args {
                game_args.push(arg.clone());
            } else {
                target_parts.push(arg.clone());
                if arg.starts_with('-') || arg.starts_with('/') {
                    let switch = target_parts.pop().unwrap();
                    game_args.push(switch);
                    found_args = true;
                } else if arg.to_lowercase().ends_with(".exe") {
                    found_args = true;
                }
            }
        }
    }

    let target_path = target_parts.join(" ");

    let state = Arc::new(LauncherState {
        progress: AtomicU32::new(0.0f32.to_bits()),
        finished: AtomicBool::new(false),
        error_message: std::sync::Mutex::new(None),
    });

    let state_clone = Arc::clone(&state);
    let handle = if !is_manual {
        Some(thread::spawn(move || {
            // Priority 1: Check HKLM Registry (Secure)
            let mut revive_path = None;
            unsafe {
                let mut hkey = HKEY::default();
                let subkey_wide: Vec<u16> = SETTINGS_PATH.encode_utf16().chain(Some(0)).collect();
                if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(subkey_wide.as_ptr()), None, KEY_READ, &mut hkey) == ERROR_SUCCESS {
                    let value_name: Vec<u16> = "ReviveInjectorPath".encode_utf16().chain(Some(0)).collect();
                    let mut buffer = [0u16; 512];
                    let mut size = (buffer.len() * 2) as u32;
                    if RegQueryValueExW(hkey, PCWSTR(value_name.as_ptr()), None, None, Some(buffer.as_mut_ptr() as *mut u8), Some(&mut size)) == ERROR_SUCCESS {
                        revive_path = Some(String::from_utf16_lossy(&buffer[..((size / 2) as usize - 1)]));
                    }
                    let _ = RegCloseKey(hkey);
                }
            }

            // Priority 2: Fallbacks (Hardcoded trusted paths)
            if revive_path.is_none() {
                let fallbacks = [
                    r#"C:\Program Files\Revive\Revive\ReviveInjector.exe"#,
                    r#"C:\Program Files\Revive\Revive\x64\ReviveInjector.exe"#,
                    r#"C:\Program Files\Revive\ReviveInjector.exe"#,
                ];
                for fallback in fallbacks {
                    if Path::new(fallback).exists() {
                        revive_path = Some(fallback.to_string());
                        break;
                    }
                }
            }

            // Priority 3: .env file (Least Secure, needs strict validation)
            if revive_path.is_none() {
                if let Some(env_path) = read_env_var_from_file("C3_REVIVE_INJECTOR_PATH") {
                    // Path Validation: Must be rooted in C:\Program Files\Revive OR have strict permissions
                    let is_in_trusted_dir = env_path.to_lowercase().starts_with(r"c:\program files\revive");
                    if is_in_trusted_dir || has_strict_permissions(Path::new(&env_path)) {
                        revive_path = Some(env_path);
                    } else {
                        log("Warning: C3_REVIVE_INJECTOR_PATH ignored because it is not in a trusted directory and does not have strict permissions.");
                    }
                }
            }

            let revive_path = match revive_path {
                Some(p) => p,
                None => {
                    let msg = "Revive Injector not found. Please ensure Revive is installed in Program Files.".to_string();
                    log(&format!("Error: {}", msg));
                    *state_clone.error_message.lock().unwrap() = Some(msg);
                    return;
                }
            };

            log(&format!("Intercepted launch of: {}", target_path));

            // Start progress bar at 5% to show we're active
            state_clone.progress.store(0.05f32.to_bits(), Ordering::Relaxed);

            // Trigger the CondorReviveHelperService to bypass IFEO
            log("Triggering CondorReviveHelperService to bypass IFEO...");
            if let Err(e) = trigger_bypass_service() {
                let msg = format!("Failed to bypass IFEO: {}. This can happen if you recently reinstalled and haven't restarted, or if the helper service is disabled. Please restart your computer to resolve this.", e);
                log(&format!("Error: {}", msg));
                *state_clone.error_message.lock().unwrap() = Some(msg);
                return; // Stop on error
            } else {
                log("Bypass service triggered.");
            }

            if Path::new(&revive_path).exists() {
                log(&format!("Running Revive Injector: {}", revive_path));
                let mut cmd = std::process::Command::new(&revive_path);
                
                // Pass target path and args to ReviveInjector
                cmd.arg(&target_path);
                for arg in game_args {
                    cmd.arg(arg);
                }

                // Set CWD to the injector's directory
                if let Some(parent) = Path::new(&revive_path).parent() {
                    cmd.current_dir(parent);
                }

                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    // Just CREATE_NO_WINDOW now, no debug flags needed.
                    cmd.creation_flags(0x08000000);
                }

                log("Waiting for Revive Injector to initialize...");
                state_clone.progress.store(0.15f32.to_bits(), Ordering::Relaxed);
                
                let child = cmd.spawn();

                match child {
                    Ok(mut child) => {
                        log("Revive Injector started. Waiting for it to exit...");
                        match child.wait() {
                            Ok(s) => {
                                if !s.success() {
                                    let msg = format!("Revive Injector failed with exit code: {}", s);
                                    log(&format!("Error: {}", msg));
                                    *state_clone.error_message.lock().unwrap() = Some(msg);
                                } else {
                                    log("Revive Injector reported success.");
                                    state_clone.progress.store(0.50f32.to_bits(), Ordering::Relaxed);
                                }
                            }
                            Err(e) => {
                                let msg = format!("Failed to wait for Revive Injector: {}", e);
                                log(&format!("Error: {}", msg));
                                *state_clone.error_message.lock().unwrap() = Some(msg);
                            }
                        }
                    }
                    Err(e) => {
                        let msg = format!("Failed to run Revive Injector: {}", e);
                        log(&format!("Error: {}", msg));
                        *state_clone.error_message.lock().unwrap() = Some(msg);
                        return; // Stop on error
                    }
                }
            } else {
                // Already checked path existence, but good for completeness.
                let msg = format!("Revive Injector not found at {}", revive_path);
                log(&format!("Error: {}", msg));
                *state_clone.error_message.lock().unwrap() = Some(msg);
                return; // Stop on error
            }

            // Wait for stabilization
            log("Waiting 3 seconds for stabilization...");
            let start_time = Instant::now();
            let base_progress = 0.50f32;
            let wait_duration = Duration::from_secs(3);
            while start_time.elapsed() < wait_duration {
                let p = base_progress + (start_time.elapsed().as_secs_f32() / wait_duration.as_secs_f32()) * (1.0 - base_progress);
                state_clone
                    .progress
                    .store(p.to_bits(), Ordering::Relaxed);
                thread::sleep(Duration::from_millis(50));
            }
            state_clone
                .progress
                .store(1.0f32.to_bits(), Ordering::Relaxed);

            log("Done. Launcher exiting.");
            state_clone.finished.store(true, Ordering::Relaxed);
        }))
    } else {
        None
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, if is_manual { 220.0 } else { 180.0 }])
            .with_always_on_top()
            .with_decorations(true)
            .with_close_button(!is_manual)
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_resizable(false),
        ..Default::default()
    };

    let result = eframe::run_native(
        "Condor VR Launcher",
        options,
        Box::new(|_cc| Ok(Box::new(LauncherApp::new(state, is_manual)))),
    );

    if let Some(h) = handle {
        let _ = h.join();
    }
    result
}
