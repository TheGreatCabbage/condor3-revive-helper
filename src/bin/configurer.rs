use std::env;
use std::io::{self, Write};
use std::fs::File;
use std::path::PathBuf;
use winreg::RegKey;
use winreg::enums::*;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Security::*;
use windows::Win32::Security::Authorization::*;
use windows::Win32::System::Services::*;

use condor3_revive_helper::{
    find_revive_injector, get_companion_exe_path, get_secure_log_path, handle_version_args,
    IFEO_PATH, LAUNCHER_EXE_NAME, SERVICE_NAME, SETTINGS_PATH, TARGET_EXE, update_condor_setup_ini,
};

fn get_local_secure_log_path() -> PathBuf {
    get_secure_log_path("CondorVR", "setup.log")
}

struct Logger {
    file: Option<File>,
}

impl Logger {
    fn new(path: &std::path::Path) -> Self {
        let file = File::create(path).ok();
        Self { file }
    }

    fn log(&mut self, msg: &str) {
        println!("{}", msg);
        if let Some(ref mut f) = self.file {
            let _ = writeln!(f, "{}", msg);
        }
    }

    fn error(&mut self, msg: &str) {
        eprintln!("{}", msg);
        if let Some(ref mut f) = self.file {
            let _ = writeln!(f, "ERROR: {}", msg);
        }
    }
}

fn main() -> io::Result<()> {
    if handle_version_args("Condor-VR-Configurer") {
        return Ok(());
    }

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage: Condor-VR-Configurer.exe [activate|deactivate]");
        return Ok(());
    }

    let log_path = get_local_secure_log_path();
    let mut logger = Logger::new(&log_path);

    let command = &args[1];
    let res = run_command(command, &mut logger);

    if let Err(e) = res {
        logger.error(&format!("Fatal error: {}", e));
        return Err(e);
    }

    Ok(())
}

fn run_command(command: &str, logger: &mut Logger) -> io::Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let target_key_path = format!(r#"{}\{}"#, IFEO_PATH, TARGET_EXE);

    match command {
        "activate" => {
            let launcher_path = get_companion_exe_path(LAUNCHER_EXE_NAME)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Launcher not found"))?;
            let launcher_path_str = launcher_path.to_str().expect("Invalid path");

            // Install or update Service
            let service_path = get_companion_exe_path(&format!("{SERVICE_NAME}.exe"))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Service not found"))?;
            let service_path_str = service_path.to_str().expect("Invalid path");

            let mut service_ok = false;
            match install_service(service_path_str) {
                Ok(_) => {
                    logger.log("Service installed.");
                    service_ok = true;
                }
                Err(e) if e.code() == ERROR_SERVICE_EXISTS.to_hresult() => {
                    logger.log("Service already exists, updating configuration...");
                    if let Err(e) = update_service_config(service_path_str) {
                        logger.error(&format!("Failed to update service config: {}", e));
                    } else {
                        service_ok = true;
                    }
                }
                Err(e) => logger.error(&format!("Failed to install service: {}", e)),
            }

            if service_ok {
                if let Err(e) = allow_everyone_to_start_service() {
                    logger.error(&format!("Failed to set service permissions: {}", e));
                } else {
                    logger.log("Service permissions set.");
                }

                let (target_key, _) =
                    hklm.create_subkey_with_flags(&target_key_path, KEY_ALL_ACCESS)?;
                let launcher_command = format!("\"{}\"", launcher_path_str);
                target_key.set_value("Debugger", &launcher_command)?;
                logger.log(&format!("Hook activated with: {}", launcher_command));

                // Store ReviveInjector path in HKLM/Software/CondorVR
                if let Some(revive_path) = find_revive_injector() {
                    if let Ok((settings_key, _)) = hklm.create_subkey_with_flags(SETTINGS_PATH, KEY_ALL_ACCESS) {
                        let _ = settings_key.set_value("ReviveInjectorPath", &revive_path);
                        logger.log(&format!("Stored ReviveInjector path: {}", revive_path));
                    }
                } else {
                    logger.error("Warning: ReviveInjector.exe not found. You may need to install Revive.");
                }

                // Update Condor Setup.ini files to enable VR
                let results = update_condor_setup_ini(true);
                for (name, success) in results {
                    if success {
                        logger.log(&format!("Updated Setup.ini for: {}", name));
                    } else {
                        logger.error(&format!("Failed to update Setup.ini for: {}", name));
                    }
                }
            } else {
                logger.error("Error: VR support could not be activated because the helper service could not be installed.");
                logger.error("This often happens if you recently uninstalled and haven't restarted yet.");
                logger.error("Please restart your computer and try again.");
            }
        }
        "deactivate" => {
            if let Ok(key) = hklm.open_subkey_with_flags(&target_key_path, KEY_ALL_ACCESS) {
                let _ = key.delete_value("Debugger");
                logger.log("Hook deactivated.");
            }

            if let Err(e) = uninstall_service(SERVICE_NAME) {
                logger.error(&format!("Failed to uninstall {}: {}", SERVICE_NAME, e));
            } else {
                logger.log(&format!("Service {} uninstalled.", SERVICE_NAME));
            }

            // Update Condor Setup.ini files to disable VR
            let results = update_condor_setup_ini(false);
            for (name, success) in results {
                if success {
                    logger.log(&format!("Updated Setup.ini for: {}", name));
                } else {
                    logger.error(&format!("Failed to update Setup.ini for: {}", name));
                }
            }
        }
        _ => logger.log(&format!("Unknown command: {}", command)),
    }

    Ok(())
}

fn update_service_config(path: &str) -> Result<(), windows::core::Error> {
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)?;
        let service_name_w: Vec<u16> = SERVICE_NAME.encode_utf16().chain(Some(0)).collect();
        let mut service_res =
            OpenServiceW(scm, PCWSTR(service_name_w.as_ptr()), SERVICE_CHANGE_CONFIG);

        if let Err(ref e) = service_res
            && e.code() == ERROR_ACCESS_DENIED.to_hresult() {
            // Try to fix permissions and then try again
            let _ = allow_everyone_to_start_service();
            service_res =
                OpenServiceW(scm, PCWSTR(service_name_w.as_ptr()), SERVICE_CHANGE_CONFIG);
        }

        let service = service_res?;

        let service_path_w: Vec<u16> = format!("\"{}\"", path)
            .encode_utf16()
            .chain(Some(0))
            .collect();

        let res = ChangeServiceConfigW(
            service,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            SERVICE_START_TYPE(SERVICE_NO_CHANGE),
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR(service_path_w.as_ptr()),
            None,
            None,
            None,
            None,
            None,
            None,
        );

        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);
        res
    }
}

fn install_service(path: &str) -> Result<(), windows::core::Error> {
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)?;
        if scm.is_invalid() {
            return Err(windows::core::Error::from_thread());
        }

        let service_name_w: Vec<u16> = SERVICE_NAME.encode_utf16().chain(Some(0)).collect();
        let service_path_w: Vec<u16> = format!("\"{}\"", path)
            .encode_utf16()
            .chain(Some(0))
            .collect();

        let service = CreateServiceW(
            scm,
            PCWSTR(service_name_w.as_ptr()),
            PCWSTR(service_name_w.as_ptr()),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_DEMAND_START,
            SERVICE_ERROR_NORMAL,
            PCWSTR(service_path_w.as_ptr()),
            None,
            None,
            None,
            None,
            None,
        );

        match service {
            Ok(s) => {
                let _ = CloseServiceHandle(s);
                let _ = CloseServiceHandle(scm);
                Ok(())
            }
            Err(e) => {
                let _ = CloseServiceHandle(scm);
                Err(e)
            }
        }
    }
}

fn uninstall_service(name: &str) -> Result<(), windows::core::Error> {
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)?;
        if scm.is_invalid() {
            return Err(windows::core::Error::from_thread());
        }

        let service_name_w: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut service_res =
            OpenServiceW(scm, PCWSTR(service_name_w.as_ptr()), SERVICE_ALL_ACCESS);

        if let Err(ref e) = service_res
            && e.code() == ERROR_ACCESS_DENIED.to_hresult() {
            // Try to fix permissions and then try again
            let _ = allow_everyone_to_start_service();
            service_res = OpenServiceW(scm, PCWSTR(service_name_w.as_ptr()), SERVICE_ALL_ACCESS);
        }

        if let Ok(service) = service_res {
            let mut status = SERVICE_STATUS::default();
            let _ = ControlService(service, SERVICE_CONTROL_STOP, &mut status);

            // Wait for service to stop
            for _ in 0..50 {
                let mut status = SERVICE_STATUS::default();
                if QueryServiceStatus(service, &mut status).is_ok()
                    && status.dwCurrentState == SERVICE_STOPPED {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            let _ = DeleteService(service);
            let _ = CloseServiceHandle(service);
        }

        let _ = CloseServiceHandle(scm);
        Ok(())
    }
}

fn allow_everyone_to_start_service() -> Result<(), windows::core::Error> {
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_CONNECT)?;
        let service_name_w: Vec<u16> = SERVICE_NAME.encode_utf16().chain(Some(0)).collect();
        // WRITE_DAC (0x00040000) is required to set security
        let service = OpenServiceW(scm, PCWSTR(service_name_w.as_ptr()), 0x00040000)?;

        // SDDL for:
        // - Local System (SY): Generic All (GA)
        // - Built-in Administrators (BA): Generic All (GA)
        // - Authenticated Users (AU): Start (RP) and Query Status (LC)
        let sddl = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;RPLC;;;AU)";
        let sddl_w: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();

        let mut p_sd: PSECURITY_DESCRIPTOR = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl_w.as_ptr()),
            SDDL_REVISION_1,
            &mut p_sd,
            None,
        )?;

        let res = SetServiceObjectSecurity(
            service,
            DACL_SECURITY_INFORMATION,
            p_sd,
        );

        let _ = LocalFree(Some(HLOCAL(p_sd.0)));
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);

        res
    }
}
