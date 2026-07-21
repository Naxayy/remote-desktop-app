//! Ejemplo standalone: prueba la inyeccion de input.
//! Te da 3 segundos para poner el foco donde quieras (ej: el Bloc de
//! notas) y despues mueve el mouse, hace un click, y escribe un texto
//! de prueba. Sin red todavia - solo para validar que SendInput
//! funciona en tu maquina antes de conectarlo al agent real.
//!
//! Correr con:
//!   cargo run --example test_input --release

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use core_engine::input::InputInjector;
    use std::thread::sleep;
    use std::time::Duration;

    println!("Tenes 3 segundos para poner el foco en una ventana de texto (ej: el Bloc de notas)...");
    sleep(Duration::from_secs(3));

    let injector = InputInjector::new();

    println!("Moviendo el mouse...");
    injector.move_mouse_absolute(400, 300)?;
    sleep(Duration::from_millis(300));

    println!("Escribiendo texto de prueba...");
    for ch in "Hola, esto es una prueba de input remoto! 🚀".chars() {
        injector.type_char(ch)?;
        sleep(Duration::from_millis(30));
    }

    println!("Listo.");
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("este ejemplo solo corre en Windows (usa SendInput)");
}
