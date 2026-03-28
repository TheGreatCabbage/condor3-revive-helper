//! This service allows the IFEO registry key, which makes Condor.exe defer to our launcher, to be 
//! deleted while the launcher runs and then re-enabled after Condor is launched via ReviveInjector. 
//! This prevents an infinite loop of the launcher being executed. 

use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

use condor3_revive_helper::{
    get_companion_exe_path, handle_version_args, IFEO_PATH, LAUNCHER_EXE_NAME, SERVICE_NAME,
    TARGET_EXE,
};

define_windows_service!(ffi_service_main, service_main);

fn main() -> Result<(), windows_service::Error> {
    if handle_version_args("CondorReviveHelperService") {
        return Ok(());
    }

    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(_e) = run_service() {
        // Log error? For now just exit.
    }
}

fn run_service() -> Result<(), Box<dyn std::error::Error>> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop => {
                r.store(false, Ordering::SeqCst);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: windows_service::service::ServiceState::StartPending,
        controls_accepted: windows_service::service::ServiceControlAccept::empty(),
        exit_code: windows_service::service::ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    })?;

    // Core logic
    // 1. Delete IFEO hook
    if let Err(e) = delete_ifeo_hook() {
        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: windows_service::service::ServiceState::Stopped,
            controls_accepted: windows_service::service::ServiceControlAccept::empty(),
            exit_code: windows_service::service::ServiceExitCode::Win32(1),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;
        return Err(e);
    }

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: windows_service::service::ServiceState::Running,
        controls_accepted: windows_service::service::ServiceControlAccept::STOP,
        exit_code: windows_service::service::ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    // 2. Wait for Condor.exe or stop signal
    while running.load(Ordering::SeqCst) && !is_process_running(TARGET_EXE) {
        thread::sleep(Duration::from_millis(500));
    }

    // 3. Restore IFEO hook (always restore if we are exiting normally or by Condor.exe detection)
    // We only skip if we were stopped and didn't even get to the running state, 
    // but actually it's safer to always try to restore it if we successfully deleted it.
    let _ = restore_ifeo_hook();

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: windows_service::service::ServiceState::Stopped,
        controls_accepted: windows_service::service::ServiceControlAccept::empty(),
        exit_code: windows_service::service::ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    Ok(())
}

fn delete_ifeo_hook() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let hklm = HKEY_LOCAL_MACHINE;
        let subkey_path = format!(r"{}\{}", IFEO_PATH, TARGET_EXE);
        let subkey_wide: Vec<u16> = subkey_path.encode_utf16().chain(Some(0)).collect();

        let mut hkey = Default::default();
        let res = RegCreateKeyExW(
            hklm,
            windows::core::PCWSTR(subkey_wide.as_ptr()),
            None,
            windows::core::PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &mut hkey,
            None,
        );

        if res != windows::Win32::Foundation::ERROR_SUCCESS {
            return Err(format!("RegCreateKeyExW failed with code: {}", res.0).into());
        }

        if !hkey.is_invalid() {
            let value_name: Vec<u16> = "Debugger".encode_utf16().chain(Some(0)).collect();
            let res = RegDeleteValueW(hkey, windows::core::PCWSTR(value_name.as_ptr()));
            let _ = windows::Win32::System::Registry::RegCloseKey(hkey);

            if res != windows::Win32::Foundation::ERROR_SUCCESS && res != windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
                return Err(format!("RegDeleteValueW failed with code: {}", res.0).into());
            }
        }
    }
    Ok(())
}

fn restore_ifeo_hook() -> Result<(), Box<dyn std::error::Error>> {
    let launcher_path = get_companion_exe_path(LAUNCHER_EXE_NAME).ok_or("Launcher not found")?;
    let launcher_path_str = launcher_path.to_str().ok_or("Invalid path")?;
    let launcher_command = format!("\"{}\"", launcher_path_str);

    unsafe {
        let hklm = HKEY_LOCAL_MACHINE;
        let subkey_path = format!(r"{}\{}", IFEO_PATH, TARGET_EXE);
        let subkey_wide: Vec<u16> = subkey_path.encode_utf16().chain(Some(0)).collect();

        let mut hkey = Default::default();
        let res = RegCreateKeyExW(
            hklm,
            windows::core::PCWSTR(subkey_wide.as_ptr()),
            None,
            windows::core::PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &mut hkey,
            None,
        );

        if res == windows::Win32::Foundation::ERROR_SUCCESS && !hkey.is_invalid() {
            let value_name: Vec<u16> = "Debugger".encode_utf16().chain(Some(0)).collect();
            let value_data: Vec<u16> = launcher_command.encode_utf16().chain(Some(0)).collect();

            let _ = RegSetValueExW(
                hkey,
                windows::core::PCWSTR(value_name.as_ptr()),
                None,
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    value_data.as_ptr() as *const u8,
                    value_data.len() * 2,
                )),
            );
            let _ = windows::Win32::System::Registry::RegCloseKey(hkey);
        }
    }
    Ok(())
}

fn is_process_running(process_name: &str) -> bool {
    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return false,
        };

        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let current_name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                
                if current_name.eq_ignore_ascii_case(process_name) {
                    let _ = CloseHandle(snapshot);
                    return true;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    false
}
