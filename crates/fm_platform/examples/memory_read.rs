// Developer-only map verification. The production reader checks the process, build and readable
// image before resolution. This tool never installs a map or enables a write adapter.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use fm_memory::{Map, Runtime};
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().ok_or("usage: memory_read <pid> <development-map.json>")?.parse()?;
    let file = args.next().ok_or("missing development map")?;
    if args.next().is_some() { return Err("unexpected argument".into()); }
    let map: Map = serde_json::from_slice(&std::fs::read(file)?)?;
    let reader = fm_platform::memory::Reader::open(pid, &map.game, &map.module)?;
    let image = reader.snapshot()?;
    let resolved = map.resolve(&image)?;
    let runtime = Runtime::new(reader, vec![resolved])?;
    println!("build={} session={}", image.id.sha256, image.id.session);
    for symbol in &map.symbols {
        let key = format!("{}/{}", map.id, symbol.name);
        println!("mem:{key}={:?}", runtime.read(&key)?);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() { eprintln!("Memory verification requires Windows."); std::process::exit(1); }
