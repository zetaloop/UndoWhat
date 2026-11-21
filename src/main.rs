mod abi;
mod download;
mod inject;
mod notify;
mod symbols;

use symbols::Preparation;

fn start() -> std::io::Result<()> {
    let mut preparation = Preparation::Cached;
    let result = symbols::resolve(&mut preparation).and_then(|symbols| {
        if matches!(preparation, Preparation::Resolve) {
            preparation = Preparation::Activate;
        }
        inject::run(Some(&symbols))
    });
    let (title, body) = match (&preparation, &result) {
        (Preparation::Cached, _) => return result,
        (_, Ok(())) => ("UndoWhat 已就绪", String::new()),
        (Preparation::Download(file), Err(error)) => {
            ("UndoWhat 符号下载失败", format!("{file}\n{error}"))
        }
        (Preparation::Resolve, Err(error)) => ("UndoWhat 符号解析失败", error.to_string()),
        (Preparation::Activate, Err(error)) => ("UndoWhat 启动失败", error.to_string()),
    };
    notify::with_apartment(|| {
        if let Err(error) = notify::show(title, &body) {
            eprintln!("{error}");
        }
    });
    result
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("inspect") => {
            for (name, rva) in symbols::resolve(&mut Preparation::Cached)?.entries() {
                println!("{name}: {rva:#x}");
            }
        }
        Some("stop") => inject::run(None)?,
        Some("start") | None => start()?,
        _ => eprintln!("Usage: undowhat [start|stop|inspect]"),
    }
    Ok(())
}
