//! The GUI program which allows the user to enable/disable VR for Condor.

#![windows_subsystem = "windows"]

use eframe::egui;
use std::env;
use winreg::RegKey;
use winreg::enums::*;
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use directories::UserDirs;
use std::path::{PathBuf};
use ini::Ini;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_OK, MB_ICONERROR, SW_HIDE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOCLOSEPROCESS};
use windows::Win32::System::Threading::{WaitForSingleObject, GetExitCodeProcess, INFINITE};
use windows::Win32::Foundation::CloseHandle;

// The name of the Condor executable.
const TARGET_EXE: &str = "Condor.exe";

fn show_error(msg: &str) {
    unsafe {
        let _ = MessageBoxW(
            None,
            &HSTRING::from(msg),
            &HSTRING::from("Error"),
            MB_OK | MB_ICONERROR,
        );
    }
}

// The registry path at which we can create a hook which will cause Conder.exe to open our launcher instead.
const IFEO_PATH: &str =
    r#"Software\Microsoft\Windows NT\CurrentVersion\Image File Execution Options"#;

fn main() -> eframe::Result {
    let args: Vec<String> = env::args().collect();
    if args.contains(&"--version".to_string()) || args.contains(&"-v".to_string()) {
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
        println!("Condor3 Revive Helper version {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([500.0, 450.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Condor3 Revive Helper",
        options,
        Box::new(|_cc| Ok(Box::new(ReviveHelperApp::default()))),
    )
}

struct PilotStatus {
    name: String,
    path: PathBuf,
    vr_enabled: bool,
}

struct ReviveHelperApp {
    is_active: bool,
    pilots: Vec<PilotStatus>,
    status_msg: String,
    logs: String,
    show_logs: bool,
}

impl Default for ReviveHelperApp {
    fn default() -> Self {
        let mut slf = Self {
            is_active: false,
            pilots: Vec::new(),
            status_msg: "Initializing...".to_string(),
            logs: String::new(),
            show_logs: false,
        };
        slf.refresh_status();
        slf
    }
}

impl ReviveHelperApp {
    fn get_setup_path(&self) -> Option<String> {
        let mut p = env::current_exe().ok()?;
        p.pop();
        p.push("Condor-VR-Configurer.exe");
        Some(p.to_str()?.to_string())
    }

    fn refresh_status(&mut self) {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let key_path = format!(r#"{}\{}"#, IFEO_PATH, TARGET_EXE);

        match hklm.open_subkey(&key_path) {
            Ok(key) => match key.get_value::<String, _>("Debugger") {
                Ok(_) => {
                    self.is_active = true;
                    self.status_msg = "Condor will launch with Revive.".to_string();
                }
                Err(_) => {
                    self.is_active = false;
                    self.status_msg = "Condor will launch without Revive.".to_string();
                }
            },
            Err(_) => {
                self.is_active = false;
                self.status_msg = "Condor will launch without Revive.".to_string();
            }
        }

        // Pilot status
        self.pilots.clear();
        if let Some(user_dirs) = UserDirs::new() {
            if let Some(docs) = user_dirs.document_dir() {
                for condor_dir in &["Condor", "Condor3"] {
                    let base_dir = docs.join(condor_dir);
                    
                    // Check global Setup.ini
                    let global_setup = base_dir.join("Setup.ini");
                    if global_setup.exists() {
                        let mut vr_enabled = false;
                        if let Ok(conf) = Ini::load_from_file(&global_setup) {
                            if let Some(section) = conf.section(Some("Graphics")) {
                                if let Some(val) = section.get("VROculusRift") {
                                    vr_enabled = val.trim() == "1";
                                }
                            }
                        }
                        self.pilots.push(PilotStatus {
                            name: format!("Global Settings ({})", condor_dir),
                            path: global_setup,
                            vr_enabled,
                        });
                    }

                    // Check pilots
                    let pilots_dir = base_dir.join("Pilots");
                    if let Ok(entries) = std::fs::read_dir(pilots_dir) {
                        for entry in entries.flatten() {
                            if entry.path().is_dir() {
                                let pilot_name = entry.file_name().to_string_lossy().into_owned();
                                let setup_ini = entry.path().join("Setup.ini");
                                if setup_ini.exists() {
                                    let mut vr_enabled = false;
                                    if let Ok(conf) = Ini::load_from_file(&setup_ini) {
                                        if let Some(section) = conf.section(Some("Graphics")) {
                                            if let Some(val) = section.get("VROculusRift") {
                                                vr_enabled = val.trim() == "1";
                                            }
                                        }
                                    }
                                    self.pilots.push(PilotStatus {
                                        name: format!("Pilot: {} ({})", pilot_name, condor_dir),
                                        path: setup_ini,
                                        vr_enabled,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn toggle_hook(&mut self) {
        // Refresh status first to ensure we have the latest pilot list and hook state
        self.refresh_status();

        self.logs.clear();
        self.show_logs = false;

        // Determine new VR value for Setup.ini files
        let new_vr_val = if self.is_active { "0" } else { "1" };
        let mut configurer_success = false;
        
        if let Some(setup_path) = self.get_setup_path() {
            let action = if self.is_active {
                "deactivate"
            } else {
                "activate"
            };

            // Use secure log path in ProgramData
            let mut log_path = if let Some(pd) = env::var_os("ProgramData") {
                PathBuf::from(pd)
            } else {
                PathBuf::from("C:\\ProgramData")
            };
            log_path.push("CondorVR");
            log_path.push("setup.log");

            // Use native Windows API ShellExecuteExW to trigger UAC elevation
            let mut sei = SHELLEXECUTEINFOW::default();
            sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            sei.fMask = SEE_MASK_NOCLOSEPROCESS;
            sei.lpVerb = windows::core::w!("runas");
            
            let path_w = HSTRING::from(&setup_path);
            sei.lpFile = PCWSTR(path_w.as_ptr());
            
            let action_w = HSTRING::from(action);
            sei.lpParameters = PCWSTR(action_w.as_ptr());
            
            sei.nShow = SW_HIDE.0 as i32;

            let success = unsafe { ShellExecuteExW(&mut sei) }.is_ok();

            if success {
                unsafe {
                    let _ = WaitForSingleObject(sei.hProcess, INFINITE);
                    let mut exit_code = 0u32;
                    let _ = GetExitCodeProcess(sei.hProcess, &mut exit_code);
                    let _ = CloseHandle(sei.hProcess);

                    if exit_code == 0 {
                        println!("Successfully executed setup with action: {}", action);
                        self.logs.push_str(&format!("Successfully executed setup with action: {}\n", action));
                        configurer_success = true;
                    } else {
                        println!("Setup exited with error status: {}", exit_code);
                        self.logs.push_str(&format!("Setup exited with error status: {}\n", exit_code));
                        self.show_logs = true;
                    }
                }
            } else {
                let err = std::io::Error::last_os_error();
                println!("Failed to execute setup: {}", err);
                self.logs.push_str(&format!("Failed to execute setup: {}\n", err));
                self.show_logs = true;
            }

            // Read log file back
            if let Ok(l) = std::fs::read_to_string(&log_path) {
                if !l.is_empty() {
                    self.logs.push_str("\n--- Setup Logs ---\n");
                    self.logs.push_str(&l);
                    if l.contains("ERROR:") {
                        self.show_logs = true;
                        configurer_success = false; // Override success if the log contains errors
                    }
                }
            }
            let _ = std::fs::remove_file(log_path);
        }

        if configurer_success {
            // Now toggle INI files for all pilots and global settings
            for pilot in &self.pilots {
                let mut conf = match Ini::load_from_file(&pilot.path) {
                    Ok(c) => c,
                    Err(_) => {
                        continue;
                    }
                };
                
                // Set VROculusRift to the target value in the Graphics section
                conf.with_section(Some("Graphics"))
                    .set("VROculusRift", new_vr_val);

                if let Err(e) = conf.write_to_file(&pilot.path) {
                    show_error(&format!("Failed to write to {}: {}", pilot.path.display(), e));
                    self.logs.push_str(&format!("Failed to write to {}: {}\n", pilot.path.display(), e));
                    self.show_logs = true;
                } else {
                    self.logs.push_str(&format!("Updated {}.\n", pilot.path.display()));
                }
            }
        } else {
            self.logs.push_str("\nSkipping Setup.ini updates because the service configuration failed.\n");
        }

        self.refresh_status();
    }
}

impl eframe::App for ReviveHelperApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Condor3 Revive Helper");
            ui.add_space(10.0);

            let action_verb = if self.is_active { "disable" } else { "enable" };
            ui.add(egui::Label::new(format!(
                "Welcome to the Revive Helper for Condor3 (and Condor2).\n\nYou can choose to {} VR for Condor below. It will take effect whenever you launch Condor, including from the Server List.",
                action_verb
            )).wrap());
            ui.add_space(20.0);
            
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label("Current status: Condor will launch ");
                if self.is_active {
                    ui.label(egui::RichText::new("with Revive").color(egui::Color32::GREEN));
                } else {
                    ui.label(egui::RichText::new("without Revive").color(egui::Color32::RED));
                }
                ui.label(".");
            });
            ui.add_space(10.0);

            ui.group(|ui| {
                ui.set_min_height(100.0);
                ui.label(egui::RichText::new("Condor Settings & Pilots:").strong());
                if self.pilots.is_empty() {
                    ui.label(egui::RichText::new("No Setup.ini files found in Documents/Condor or Documents/Condor3.").weak());
                } else {
                    egui::ScrollArea::vertical().id_salt("pilot_scroll").show(ui, |ui| {
                        for pilot in &self.pilots {
                            ui.horizontal(|ui| {
                                ui.label(format!("{}:", pilot.name));
                                if pilot.vr_enabled {
                                    ui.label(egui::RichText::new("VR Enabled").color(egui::Color32::GREEN));
                                } else {
                                    ui.label(egui::RichText::new("VR Disabled").color(egui::Color32::RED));
                                }
                            });
                        }
                    });
                }
            });

            ui.add_space(10.0);

            let button_text = if self.is_active { "Disable VR" } else { "Enable VR" };
            if ui.add_sized([120.0, 40.0], egui::Button::new(button_text)).clicked() {
                self.toggle_hook();
            }
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Tip: Toggling the VR setting will open a permission dialog and update all pilots' Setup.ini.").weak());

            if !self.logs.is_empty() {
                ui.add_space(10.0);
                if ui.button("View Execution Logs").clicked() {
                    self.show_logs = true;
                }
            }

            if self.show_logs {
                let mut is_open = self.show_logs;
                let mut clear_clicked = false;
                egui::Window::new("Execution Logs")
                    .open(&mut is_open)
                    .default_size([600.0, 400.0])
                    .resizable(true)
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("log_scroll")
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(&self.logs).monospace())
                                        .wrap()
                                );
                            });
                        ui.add_space(10.0);
                        if ui.button("Clear & Close").clicked() {
                            clear_clicked = true;
                        }
                    });
                
                if clear_clicked {
                    self.logs.clear();
                    self.show_logs = false;
                } else {
                    self.show_logs = is_open;
                }
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                ui.add_space(10.0);
                if ui.add_sized([80.0, 30.0], egui::Button::new("Exit")).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                ui.add_space(5.0);
                if ui.add_sized([80.0, 30.0], egui::Button::new("Refresh")).clicked() {
                    self.refresh_status();
                }
            });
        });
    }
}
