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
use windows::Win32::System::Diagnostics::Debug::*;
use windows::Win32::System::Threading::*;
use windows::core::*;

use eframe::egui;

fn log(msg: &str) {
    println!("{}", msg);

    #[cfg(feature = "logging")]
    {
        let mut log_path = env::current_exe().unwrap();
        log_path.pop();
        log_path.push("CondorVR_log.txt");

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .unwrap();

        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!("[{}] {}\n", timestamp, msg);
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
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
                    ui.label("This helper is ready and will automatically activate");
                    ui.label("whenever you launch Condor from any shortcut.");
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
                ui.add_space(10.0);

                if self.is_manual {
                    ui.add_space(10.0);
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
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

            unsafe {
                // 1. Launch Condor.exe with IFEO bypass flags
                let mut cmd_line = format!("\"{}\"", target_path);
                for arg in game_args {
                    cmd_line.push(' ');
                    if arg.contains(' ') && !arg.starts_with('"') {
                        cmd_line.push_str(&format!("\"{}\"", arg));
                    } else {
                        cmd_line.push_str(&arg);
                    }
                }
                let mut cmd_line_w: Vec<u16> =
                    cmd_line.encode_utf16().chain(std::iter::once(0)).collect();

                let mut si = STARTUPINFOW::default();
                si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
                let mut pi = PROCESS_INFORMATION::default();

                log(&format!(
                    "Starting target process with bypass: {}",
                    cmd_line
                ));
                let success = CreateProcessW(
                    None,
                    Some(PWSTR(cmd_line_w.as_mut_ptr())),
                    None,
                    None,
                    false,
                    DEBUG_ONLY_THIS_PROCESS | CREATE_SUSPENDED,
                    None,
                    None,
                    &si,
                    &mut pi,
                );

                if let Err(e) = success {
                    let msg = format!("Failed to start Condor: {}", e);
                    log(&format!("Error: {}", msg));
                    *state_clone.error_message.lock().unwrap() = Some(msg);
                } else {
                    log(&format!(
                        "Process started. PID: {}, Thread: {:?}",
                        pi.dwProcessId, pi.hThread
                    ));

                    // 2. Detach immediately to bypass IFEO while keeping it suspended
                    let _ = DebugActiveProcessStop(pi.dwProcessId);
                    log("Detached from process (IFEO bypassed).");

                    // 3. Use ReviveInjector to inject into the suspended process
                    if Path::new(&revive_path).exists() {
                        log(&format!(
                            "Running Revive Injector for PID {}: {}",
                            pi.dwProcessId, revive_path
                        ));
                        let mut cmd = std::process::Command::new(&revive_path);
                        cmd.arg("/handle").arg(pi.dwProcessId.to_string());

                        #[cfg(windows)]
                        {
                            use std::os::windows::process::CommandExt;
                            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
                        }

                        let status = cmd.status();

                        match status {
                            Ok(s) if s.success() => log("Revive Injector reported success."),
                            Ok(s) => {
                                let msg = format!("Revive Injector exited with status: {}", s);
                                log(&format!("Error: {}", msg));
                                *state_clone.error_message.lock().unwrap() = Some(msg);
                            }
                            Err(e) => {
                                let msg = format!("Failed to run Revive Injector: {}", e);
                                log(&format!("Error: {}", msg));
                                *state_clone.error_message.lock().unwrap() = Some(msg);
                            }
                        }
                    } else {
                        let msg = format!("Revive Injector not found at {}", revive_path);
                        log(&format!("Error: {}", msg));
                        *state_clone.error_message.lock().unwrap() = Some(msg);
                    }

                    // 4. Resume the process
                    log("Resuming target process...");
                    ResumeThread(pi.hThread);

                    let _ = CloseHandle(pi.hProcess);
                    let _ = CloseHandle(pi.hThread);
                }
            }

            // Wait for stabilization
            log("Waiting 3 seconds for game process to stabilize...");
            let start_time = Instant::now();
            let wait_duration = Duration::from_secs(3);
            while start_time.elapsed() < wait_duration {
                let progress = start_time.elapsed().as_secs_f32() / wait_duration.as_secs_f32();
                state_clone
                    .progress
                    .store(progress.to_bits(), Ordering::Relaxed);
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
            .with_inner_size([400.0, 180.0])
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
