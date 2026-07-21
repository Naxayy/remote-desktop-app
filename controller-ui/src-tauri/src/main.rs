// Backend Rust de la app controladora (Tauri).
// Proximo paso: exponer comandos para listar conexiones, iniciar sesion
// remota (via signaling-server) y renderizar el stream de video que
// entrega core_engine.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error corriendo la app controller-ui");
}
