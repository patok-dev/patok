use std::fs;
use std::path::Path;
use std::env;

fn main() {
    // Import the schema functions
    let daemon_schema = patok_core::config::schema::daemon_json_schema();
    let tui_schema = patok_core::config::schema::tui_json_schema();
    
    // Find the repository root by looking for Cargo.toml in parent directories
    let mut current_dir = env::current_dir().expect("Failed to get current directory");
    loop {
        if current_dir.join("Cargo.toml").exists() {
            break;
        }
        if !current_dir.pop() {
            eprintln!("Failed to find repository root");
            std::process::exit(1);
        }
    }
    let schemas_dir = current_dir.join("schemas");
    fs::create_dir_all(&schemas_dir).expect("Failed to create schemas directory");
    
    fs::write(schemas_dir.join("daemon.schema.json"), daemon_schema).expect("Failed to write daemon.schema.json");
    fs::write(schemas_dir.join("tui.schema.json"), tui_schema).expect("Failed to write tui.schema.json");
    
    println!("Schemas regenerated successfully!");
}
