//! The GUI program which allows the user to enable/disable VR for Condor.

#![windows_subsystem = "windows"]

use eframe::egui;
use std::env;
use std::process::Command;
use winreg::RegKey;
use winreg::enums::*;

// The name of the Condor executable.
const TARGET_EXE: &str = "Condor.exe";

// The registry path at which we can create a hook which will cause Conder.exe to open our launcher instead.
const IFEO_PATH: &str =
    r#"Software\Microsoft\Windows NT\CurrentVersion\Image File Execution Options"#;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([450.0, 280.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Condor3 Revive Helper",
        options,
        Box::new(|_cc| Ok(Box::new(ReviveHelperApp::default()))),
    )
}

struct ReviveHelperApp {
    is_active: bool,
    status_msg: String,
}

impl Default for ReviveHelperApp {
    fn default() -> Self {
        let mut slf = Self {
            is_active: false,
            status_msg: "Initializing...".to_string(),
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
                    self.status_msg = "Condor will launch with VR enabled.".to_string();
                }
                Err(_) => {
                    self.is_active = false;
                    self.status_msg = "Condor will launch with VR disabled.".to_string();
                }
            },
            Err(_) => {
                self.is_active = false;
                self.status_msg = "Condor will launch with VR disabled.".to_string();
            }
        }
    }

    fn toggle_hook(&mut self) {
        if let Some(setup_path) = self.get_setup_path() {
            let action = if self.is_active {
                "deactivate"
            } else {
                "activate"
            };

            // Use PowerShell to trigger UAC elevation via Start-Process -Verb RunAs
            let mut command = Command::new("powershell");

            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }

            let status = command
                .args([
                    "-NoProfile",
                    "-WindowStyle",
                    "Hidden",
                    "-Command",
                    &format!(
                        "Start-Process -FilePath \"{}\" -ArgumentList \"{}\" -Verb RunAs -Wait",
                        setup_path, action
                    ),
                ])
                .status();

            match status {
                Ok(s) if s.success() => {
                    println!("Successfully executed setup with action: {}", action);
                }
                Ok(s) => println!("Setup exited with error status: {}", s),
                Err(e) => println!("Failed to execute setup: {}", e),
            }
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
                ui.label("Current status: Condor will launch with VR ");
                if self.is_active {
                    ui.label(egui::RichText::new("enabled").color(egui::Color32::GREEN));
                } else {
                    ui.label(egui::RichText::new("disabled").color(egui::Color32::RED));
                }
                ui.label(".");
            });
            ui.add_space(10.0);

            let button_text = if self.is_active { "Disable VR" } else { "Enable VR" };
            if ui.add_sized([120.0, 40.0], egui::Button::new(button_text)).clicked() {
                self.toggle_hook();
            }
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Tip: Toggling the VR setting will open a permission dialog.").weak());

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
