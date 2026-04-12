pub mod ai;
pub mod translation;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
// Comando de prueba por defecto de Tauri
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

// Punto de entrada principal para configurar y correr la aplicación Tauri
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
// Construye la aplicación registrando los plugins y manejadores de comandos (commands handler)
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            ai::validate_api_key,
            translation::translate_epub
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
