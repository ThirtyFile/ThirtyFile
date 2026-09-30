//! The program: the server is the library (lib.rs)

fn main() -> Result<(), Box<dyn std::error::Error>> {
    thirtyfile::cli::main()
}
