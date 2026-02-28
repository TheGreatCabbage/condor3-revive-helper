use std::env;
use std::io;
use winreg::RegKey;
use winreg::enums::*;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Security::*;
use windows::Win32::Security::Authorization::*;
use windows::Win32::System::Services::*;

const TARGET_EXE: &str = "Condor.exe";
const IFEO_PATH: &str =
    r#"Software\Microsoft\Windows NT\CurrentVersion\Image File Execution Options"#;
const SERVICE_NAME: &str = "CondorReviveHelperService";

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

            let (target_key, _) =
                hklm.create_subkey_with_flags(&target_key_path, KEY_ALL_ACCESS)?;
            let launcher_command = format!("\"{}\"", launcher_path_str);
            target_key.set_value("Debugger", &launcher_command)?;
            println!("Hook activated with: {}", launcher_command);

            // Install or update Service
            let mut service_path = env::current_exe()?;
            service_path.pop();
            service_path.push(format!("{SERVICE_NAME}.exe"));
            let service_path_str = service_path.to_str().expect("Invalid path");

            match install_service(service_path_str) {
                Ok(_) => println!("Service installed."),
                Err(e) if e.code() == ERROR_SERVICE_EXISTS.to_hresult() => {
                    println!("Service already exists, updating configuration...");
                    if let Err(e) = update_service_config(service_path_str) {
                        eprintln!("Failed to update service config: {}", e);
                    }
                }
                Err(e) => eprintln!("Failed to install service: {}", e),
            }

            if let Err(e) = allow_everyone_to_start_service() {
                eprintln!("Failed to set service permissions: {}", e);
            } else {
                println!("Service permissions set.");
            }
        }
        "deactivate" => {
            if let Ok(key) = hklm.open_subkey_with_flags(&target_key_path, KEY_ALL_ACCESS) {
                let _ = key.delete_value("Debugger");
                println!("Hook deactivated.");
            }

            if let Err(e) = uninstall_service(SERVICE_NAME) {
                eprintln!("Failed to uninstall {}: {}", SERVICE_NAME, e);
            } else {
                println!("Service {} uninstalled.", SERVICE_NAME);
            }
        }
        _ => println!("Unknown command: {}", command),
    }

    Ok(())
}

fn update_service_config(path: &str) -> Result<(), windows::core::Error> {
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)?;
        let service_name_w: Vec<u16> = SERVICE_NAME.encode_utf16().chain(Some(0)).collect();
        let service = OpenServiceW(scm, PCWSTR(service_name_w.as_ptr()), SERVICE_CHANGE_CONFIG)?;

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
        let service = OpenServiceW(scm, PCWSTR(service_name_w.as_ptr()), SERVICE_ALL_ACCESS);

        if let Ok(service) = service {
            let mut status = SERVICE_STATUS::default();
            let _ = ControlService(service, SERVICE_CONTROL_STOP, &mut status);
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

        // SDDL for "Allow Everyone (WD) Start (RP), Stop (WP), and Query Status (LC)"
        // D: - DACL
        // (A;;RPWPLC;;;WD) - Allow; ; Start+Stop+QueryStatus; ; ; Everyone
        let sddl = "D:(A;;RPWPLC;;;WD)";
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
