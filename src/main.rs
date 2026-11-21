mod symbols;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() != Some("inspect") {
        eprintln!("Usage: undowhat inspect");
        return Ok(());
    }

    for (name, rva) in symbols::resolve()?.entries() {
        println!("{name}: {rva:#x}");
    }

    Ok(())
}
