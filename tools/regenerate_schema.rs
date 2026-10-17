use std::fs;
use std::path::Path;

fn main() {
    // Import the schema functions
    let daemon_schema = patok_core::config::schema::daemon_json_schema();
    let tui_schema = patok_core::config::schema::tui_json_schema();
    
    let schemas_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    fs::create_dir_all(&schemas_dir).unwrap();
    
    fs::write(schemas_dir.join("daemon.schema.json"), daemon_schema).unwrap();
    fs::write(schemas_dir.join("tui.schema.json"), tui_schema).unwrap();
    
    println!("Schemas regenerated successfully!");
}
