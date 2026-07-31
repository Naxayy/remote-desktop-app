//! Manejo del agent como servicio de Windows: instalar, desinstalar,
//! y arrancar via el Service Control Manager (SCM) para que corra
//! antes del login, con privilegios de LocalSystem, y sobreviva
//! reinicios.
//!
//! Detalle importante: los servicios de Windows corren en la "sesion
//! 0", aislada por seguridad del escritorio interactivo del usuario -
//! por diseño, NINGUN servicio puede capturar pantalla directamente
//! (DXGI Desktop Duplication falla con DXGI_ERROR_NOT_CURRENTLY_
//! AVAILABLE si lo intenta). La solucion estandar (la misma que usan
//! TeamViewer/AnyDesk) es que el servicio no capture nada el mismo:
//! detecta la sesion del usuario logueado y lanza ahi un proceso
//! "worker" (este mismo .exe, en modo `console`) usando su token -
//! ese proceso SI tiene acceso al escritorio real.
//!
//! Nota: este archivo se escribio sin poder compilarlo (entorno
//! Linux) y es la parte mas dificil de todo el proyecto de probar
//! sin acceso a Windows real - es esperable necesitar varios ajustes.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, TokenPrimary, TOKEN_ALL_ACCESS,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
    PROCESS_INFORMATION, STARTUPINFOW,
};
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

pub const SERVICE_NAME: &str = "RemoteDesktopAppAgent";
const SERVICE_DISPLAY_NAME: &str = "Remote Desktop App - Agent";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

/// Sesion invalida - nadie logueado en la consola activa todavia
/// (ej: recien arranco Windows y sigue en la pantalla de login).
const INVALID_SESSION_ID: u32 = 0xFFFFFFFF;

/// Registra el binario actual como servicio de Windows: arranque
/// automatico, antes del login, corriendo como LocalSystem.
pub fn install() -> Result<()> {
    let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)
        .context("no se pudo conectar al Service Control Manager (¿corriste como administrador?)")?;

    let exe_path = std::env::current_exe().context("no se pudo obtener la ruta del ejecutable")?;

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path,
        launch_arguments: vec![],
        dependencies: vec![],
        // None = corre como LocalSystem: necesario para poder listar
        // sesiones de otros usuarios y lanzar el worker en su sesion.
        account_name: None,
        account_password: None,
    };

    let service = service_manager
        .create_service(&service_info, ServiceAccess::CHANGE_CONFIG)
        .context("no se pudo crear el servicio")?;

    let _ = service.set_description(
        "Agente de Remote Desktop App: permite conexiones remotas de soporte tecnico.",
    );

    println!("Servicio '{SERVICE_NAME}' instalado.");
    println!("Arranca solo en el proximo inicio de Windows, o corre ahora:");
    println!("  net start {SERVICE_NAME}");
    Ok(())
}

/// Para (si esta corriendo) y elimina el servicio.
pub fn uninstall() -> Result<()> {
    let manager_access = ServiceManagerAccess::CONNECT;
    let service_manager = ServiceManager::local_computer(None::<&str>, manager_access)
        .context("no se pudo conectar al Service Control Manager (¿corriste como administrador?)")?;

    let service_access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = service_manager
        .open_service(SERVICE_NAME, service_access)
        .context("no se encontro el servicio (¿ya estaba desinstalado?)")?;

    let status = service.query_status()?;
    if status.current_state != ServiceState::Stopped {
        service.stop()?;
        std::thread::sleep(Duration::from_secs(2));
    }
    service.delete()?;
    println!("Servicio '{SERVICE_NAME}' desinstalado.");
    Ok(())
}

windows_service::define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!("el servicio termino con error: {e:#}");
    }
}

fn init_service_logging() {
    let log_dir = std::path::Path::new("C:\\ProgramData\\RemoteDesktopAppAgent");
    let _ = std::fs::create_dir_all(log_dir);
    let log_path = log_dir.join("agent.log");

    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .try_init();
    }
}

/// Intenta lanzar el proceso worker (este mismo .exe, en modo
/// `console`) dentro de la sesion del usuario actualmente logueado en
/// la consola activa. Ese proceso SI tiene acceso al escritorio real
/// (a diferencia de este servicio, que corre en la sesion 0).
/// Devuelve el handle del proceso lanzado si funciono.
fn launch_session_worker(exe_path: &str) -> Option<PROCESS_INFORMATION> {
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == INVALID_SESSION_ID {
            return None; // nadie logueado todavia (ej: pantalla de login)
        }

        let mut user_token = HANDLE::default();
        if WTSQueryUserToken(session_id, &mut user_token).is_err() {
            tracing::debug!("WTSQueryUserToken fallo para la sesion {session_id}");
            return None;
        }

        let mut primary_token = HANDLE::default();
        let dup_ok = DuplicateTokenEx(
            user_token,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary_token,
        )
        .is_ok();
        let _ = CloseHandle(user_token);
        if !dup_ok {
            tracing::warn!("no se pudo duplicar el token de la sesion {session_id}");
            return None;
        }

        let mut env_block: *mut std::ffi::c_void = std::ptr::null_mut();
        let _ = CreateEnvironmentBlock(&mut env_block, primary_token, false);

        let mut desktop: Vec<u16> = "winsta0\\default\0".encode_utf16().collect();
        let mut startup_info = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };

        let mut process_info = PROCESS_INFORMATION::default();

        let mut cmd_line: Vec<u16> = format!("\"{exe_path}\" console")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let creation_flags = CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW;

        let result = CreateProcessAsUserW(
            primary_token,
            None,
            PWSTR(cmd_line.as_mut_ptr()),
            None,
            None,
            false,
            creation_flags,
            Some(env_block),
            None,
            &mut startup_info,
            &mut process_info,
        );

        if !env_block.is_null() {
            let _ = DestroyEnvironmentBlock(env_block);
        }
        let _ = CloseHandle(primary_token);

        match result {
            Ok(()) => {
                tracing::info!(
                    "worker lanzado en la sesion {session_id} (pid {})",
                    process_info.dwProcessId
                );
                let _ = CloseHandle(process_info.hThread);
                Some(process_info)
            }
            Err(e) => {
                tracing::warn!("CreateProcessAsUserW fallo: {e}");
                None
            }
        }
    }
}

/// Supervisa la sesion activa: si no hay worker corriendo (recien
/// arranco el servicio, el worker se cerro, el usuario cerro sesion y
/// volvio a entrar, etc), intenta lanzar uno nuevo. Corre hasta que
/// `should_stop` se ponga en true.
fn supervise_session_worker(exe_path: String, should_stop: Arc<AtomicBool>) {
    let mut current: Option<PROCESS_INFORMATION> = None;

    while !should_stop.load(Ordering::Relaxed) {
        let alive = current.is_some_and(|pi| unsafe {
            // timeout 0 = solo consultar el estado, sin bloquear.
            // WAIT_TIMEOUT (258) significa que el proceso sigue vivo.
            WaitForSingleObject(pi.hProcess, 0).0 == 258
        });

        if !alive {
            if let Some(pi) = current.take() {
                unsafe {
                    let _ = CloseHandle(pi.hProcess);
                }
                tracing::info!("el worker anterior ya no esta corriendo, reintentando...");
            }
            current = launch_session_worker(&exe_path);
        }

        std::thread::sleep(Duration::from_secs(3));
    }

    // Al parar el servicio, tambien cerramos el worker.
    if let Some(pi) = current {
        unsafe {
            let _ = windows::Win32::System::Threading::TerminateProcess(pi.hProcess, 0);
            let _ = CloseHandle(pi.hProcess);
        }
    }
}

fn run_service() -> Result<()> {
    init_service_logging();
    tracing::info!("servicio arrancando...");

    let should_stop = Arc::new(AtomicBool::new(false));
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    let should_stop_for_handler = Arc::clone(&should_stop);
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                should_stop_for_handler.store(true, Ordering::Relaxed);
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)
        .context("no se pudo registrar el control handler del servicio")?;

    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    let exe_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| SERVICE_NAME.to_string());

    {
        let should_stop = Arc::clone(&should_stop);
        std::thread::spawn(move || {
            supervise_session_worker(exe_path, should_stop);
        });
    }

    // Bloquea este hilo hasta que llegue Stop/Shutdown desde el SCM.
    let _ = shutdown_rx.recv();

    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    Ok(())
}

/// Intenta arrancar como servicio via el SCM. Si el proceso no fue
/// lanzado por el SCM (ej: se ejecuto el .exe directamente), esta
/// llamada falla rapido - el caller (`main`) usa eso como señal para
/// caer al modo consola.
pub fn start_dispatcher() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("no se pudo arrancar el service dispatcher")?;
    Ok(())
}
