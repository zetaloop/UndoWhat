mod abi;
mod download;
mod inject;
mod symbols;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("inspect") => {
            for (name, rva) in symbols::resolve()?.entries() {
                println!("{name}: {rva:#x}");
            }
        }
        Some("stop") => inject::run(None)?,
        Some("start") | None => inject::run(Some(&symbols::resolve()?))?,
        _ => eprintln!("Usage: undowhat [start|stop|inspect]"),
    }
    Ok(())
}
