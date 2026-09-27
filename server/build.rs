use std::path::Path;

/// Re-embed the frontend whenever anything in web/dist changes. Cargo only watches the directory entry itself for a
/// `rerun-if-changed` on a directory, so a rebuilt `dist/assets/index-*.js` (new hashed name, same directory) would be missed:
/// list every file and directory instead.
fn watch(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                watch(&p);
            } else {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }
}

fn main() {
    watch(Path::new("../web/dist"));
    watch(Path::new("migrations"));
}
