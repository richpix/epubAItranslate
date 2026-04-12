// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Punto de arranque para la aplicación; llama a la biblioteca `run` embebida
fn main() {
    epubtr_lib::run()
}
