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
use windows::core::PCWSTR;

use eframe::egui;

const BYPASS_SERVICE_NAME: &str = "CondorReviveHelperService";

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
                    // Give it a tiny bit more time to ensure the hook is actually deleted
                    thread::sleep(Duration::from_millis(200));
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);
        Ok(())
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
            let mut revive_path = if let Ok(env_path) = env::var("C3_REVIVE_INJECTOR_PATH") {
                env_path
            } else {
                r#"C:\Program Files\Revive\Revive\ReviveInjector.exe"#.to_string()
            };

            if env::var("C3_REVIVE_INJECTOR_PATH").is_err() && !Path::new(&revive_path).exists() {
                let fallbacks = [
                    r#"C:\Program Files\Revive\Revive\x64\ReviveInjector.exe"#,
                    r#"C:\Program Files\Revive\ReviveInjector.exe"#,
                ];

                for fallback in fallbacks {
                    if Path::new(fallback).exists() {
                        revive_path = fallback.to_string();
                        break;
                    }
                }
            }

            log(&format!("Intercepted launch of: {}", target_path));

            // Start progress bar at 5% to show we're active
            state_clone.progress.store(0.05f32.to_bits(), Ordering::Relaxed);

            // Trigger the CondorReviveHelperService to bypass IFEO
            log("Triggering CondorReviveHelperService to bypass IFEO...");
            if let Err(e) = trigger_bypass_service() {
                log(&format!("Warning: Failed to trigger bypass service: {}. Launch may fail if IFEO is active.", e));
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
