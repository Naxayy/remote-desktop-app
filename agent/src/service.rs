//! Manejo del agent como servicio de Windows: instalar, desinstalar,
//! y arrancar via el Service Control Manager (SCM) para que corra
//! antes del login, con privilegios de LocalSystem, y sobreviva
//! reinicios.
//!
//! Nota: este archivo se escribio sin poder compilarlo (entorno
//! Linux). Es esperable algun ajuste de API al compilarlo por primera
//! vez en Windows - mismo patron que tuvimos con capture/input.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;
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

/// Registra el binario actual como servicio de Windows: arranque
/// automatico, antes del login, corriendo como LocalSystem (necesario
/// para poder inyectar input en cualquier sesion, incluida la
/// pantalla de login). Requiere correr la terminal como administrador.
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
        // None = corre como LocalSystem: privilegios maximos, no
        // necesita que haya un usuario logueado - es justo lo que
        // permite reconectar despues de un reinicio sin intervencion.
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

/// Punto de entrada que el SCM invoca cuando arranca el servicio.
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

fn run_service() -> Result<()> {
    init_service_logging();
    tracing::info!("servicio arrancando...");

    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
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

    // El agent de verdad corre en un hilo aparte, con su propio
    // runtime de tokio - asi este hilo queda libre para atender la
    // señal de parada del SCM sin bloquearse.
    std::thread::spawn(|| {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!("no se pudo crear el runtime de tokio: {e}");
                return;
            }
        };
        if let Err(e) = runtime.block_on(crate::run_agent()) {
            tracing::error!("el agent (dentro del servicio) termino con error: {e:#}");
        }
    });

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
