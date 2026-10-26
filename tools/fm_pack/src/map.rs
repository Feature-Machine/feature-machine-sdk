// Creator tools for memory maps (plan 2026-10-20-camera-map-updates; owner: creators make and
// repair maps without us). find and check read the running game with the read-only fm_platform
// Reader and the host's fm_memory checks, so a result here is the result in the game (Windows
// only). pin writes each map digest into the feature's pins (plan 6c: no signature of ours).
// The commands and their arguments are in USAGE.
pub const USAGE: &str = "fm_pack map find <map.json> <pid> [--size WxH] | fm_pack map check <map.json> <pid> [seconds] [--size WxH] [--trace <file>] | fm_pack map pin <feature folder>";

/// The options of a map command after its plain arguments.
#[derive(Debug, Default, PartialEq)]
pub struct Options<'a> {
    /// The aspect of `--size`, else None (the window).
    pub size: Option<f32>,
    /// `--trace <file>`: one JSON line per read of `check`, with the state of each kept copy, so a
    /// failed check shows its cause (R4 run 1 on a test PC failed with no such record).
    pub trace: Option<&'a str>,
}

/// The command line of a map command: the plain arguments and the aspect of `--size`. A game in
/// full screen at a resolution other than the desktop's keeps a desktop-sized window, so the
/// window aspect is not the aspect that the game draws (live 2026-10-20); the creator then gives
/// the game's resolution. Without `--size` the tools use the window, as the host did before.
pub fn parse<'a>(args: &[&'a str]) -> Result<(Vec<&'a str>, Options<'a>), String> {
    let (mut plain, mut o) = (Vec::new(), Options::default());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--size" if o.size.is_some() => return Err("--size is given twice".into()),
            "--size" => o.size = Some(aspect_of(it.next().ok_or("--size needs a value, for example 1920x1080")?)?),
            "--trace" if o.trace.is_some() => return Err("--trace is given twice".into()),
            "--trace" => o.trace = Some(*it.next().ok_or("--trace needs a file")?),
            _ => plain.push(*a),
        }
    }
    Ok((plain, o))
}

/// Width over height of `<w>x<h>`, both whole numbers above 0.
fn aspect_of(v: &str) -> Result<f32, String> {
    let bad = || format!("--size must be <width>x<height> above 0, for example 1920x1080, not {v}");
    let (w, h) = v.split_once('x').ok_or_else(bad)?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok(w as f32 / h as f32),
        _ => Err(bad()),
    }
}

/// Routes `map find|check` after parsing; `args` starts after "map".
pub fn command(args: &[&str]) -> Result<(), String> {
    let (plain, o) = parse(args)?;
    match plain.as_slice() {
        ["find", file, pid] if o.trace.is_none() => find(file, pid, o.size),
        ["check", file, pid, rest @ ..] if rest.len() <= 1 => check(file, pid, rest.first().copied(), &o),
        ["pin", dir] if o == Options::default() => pin(std::path::Path::new(dir)).map(|_| ()),
        _ => Err(format!("usage: {USAGE}")),
    }
}

/// Writes the digest of each pinned map file (maps/<id>/<version>/map.json) into feature.json,
/// so a creator who changes a map edits no hash. Each map must pass the host body rules first.
/// Only the digest value of each pin changes, so the file keeps its layout. The "sha256" keys
/// are found in array order after the one "memory_maps" key, so equal placeholders (review
/// round 2) each get their own map. Returns the number of changed pins.
pub fn pin(dir: &std::path::Path) -> Result<usize, String> {
    let path = dir.join(fm_install::FEATURE_MANIFEST);
    let mut text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m: fm_install::feature::Manifest = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let by_hand = |what: &str| format!("{}: {what}; write the pins by hand", path.display());
    let [(mut cursor, _)] = text.match_indices("\"memory_maps\"").collect::<Vec<_>>()[..] else { return Err(by_hand("no single \"memory_maps\" key")) };
    let mut changed = 0;
    for dependency in &m.memory_maps {
        let file = dir.join(fm_install::memory::map_path(dependency));
        let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        fm_install::memory::parse_package(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
        let sha = fm_install::memory::digest(&bytes);
        println!("{}", serde_json::json!({ "id": dependency.id, "version": dependency.version, "sha256": sha, "changed": sha != dependency.sha256 }));
        // The next "sha256" key (a quoted name with a colon after it, not an id value) is this
        // pin's: an id can also be 64 hex digits (review round 3), so the value alone is not enough.
        let at = loop {
            let key = cursor + text[cursor..].find("\"sha256\"").ok_or_else(|| by_hand(&format!("no digest key for {}", dependency.id)))?;
            cursor = key + "\"sha256\"".len();
            if let Some(value) = text[cursor..].trim_start().strip_prefix(':') { break text.len() - value.trim_start().len(); }
        };
        let old = format!("\"{}\"", dependency.sha256);
        if !text[at..].starts_with(&old) { return Err(by_hand(&format!("the digest of {} is not in its pin", dependency.id))); }
        let new = format!("\"{sha}\"");
        text.replace_range(at..at + old.len(), &new);
        cursor = at + new.len();
        changed += usize::from(sha != dependency.sha256);
    }
    if changed > 0 { std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?; }
    Ok(changed)
}

#[cfg(windows)]
use win::{check, find};

#[cfg(not(windows))]
fn find(_: &str, _: &str, _: Option<f32>) -> Result<(), String> { Err("map find reads a running game on Windows only".into()) }
#[cfg(not(windows))]
fn check(_: &str, _: &str, _: Option<&str>, _: &Options) -> Result<(), String> { Err("map check reads a running game on Windows only".into()) }

#[cfg(windows)]
mod win {
    use super::Options;
    use fm_install::memory::Signed;
    use fm_memory::{CopyRead, Live, Map, Memory, Runtime, Scalar};
    use fm_platform::memory::Reader;
    use std::io::Write;
    use std::time::{Duration, Instant};

    /// One read: the camera or the error, the reason key of a miss, the kept copies (locator),
    /// and for a failed exact read its raw record (see `raw`), else null.
    struct Outcome { value: Result<Scalar, String>, reason: &'static str, copies: Vec<CopyRead>, raw: serde_json::Value }
    type Read = Box<dyn FnMut(f32) -> Outcome>;
    fn miss(e: String, reason: &'static str) -> Outcome { Outcome { value: Err(e), reason, copies: vec![], raw: serde_json::Value::Null } }

    /// The host body rules (kind, contract, one valid map or locator), not the signature: a
    /// creator checks a file before signing it.
    fn package(path: &str) -> Result<Signed, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        fm_install::memory::parse_package(&bytes).map_err(|e| format!("{path}: {e}"))
    }

    fn open(pid: &str, game: &str, module: &str) -> Result<(u32, Reader), String> {
        let pid: u32 = pid.parse().map_err(|_| format!("not a process id: {pid}"))?;
        Ok((pid, Reader::open(pid, game, module).map_err(|e| e.to_string())?))
    }

    /// The `--size` aspect, else the aspect of the game's window at this moment.
    fn aspect(pid: u32, size: Option<f32>) -> Result<f32, String> {
        size.or_else(|| fm_platform::memory::window_aspect(pid)).ok_or_else(|| "the game shows no window".to_string())
    }

    fn camera(v: &Scalar) -> serde_json::Value {
        match v {
            Scalar::Camera { near, tan_half_v, aspect, .. } => serde_json::json!({ "vfov": (2.0 * tan_half_v.atan()).to_degrees(), "near": near, "aspect": aspect }),
            other => serde_json::json!({ "value": format!("{other:?}") }),
        }
    }

    fn sleep(d: Duration) -> bool { std::thread::sleep(d); true }

    /// Runs a locator once and shows the live copies that it keeps and their camera value.
    pub fn find(path: &str, pid: &str, size: Option<f32>) -> Result<(), String> {
        let Signed::Locator(l) = package(path)? else { return Err("this map package has no locator".into()) };
        let (pid, reader) = open(pid, &l.game, &l.module)?;
        let view = aspect(pid, size)?;
        let start = Instant::now();
        let mut found = l.find(&reader, view, &mut sleep).map_err(|e| format!("no camera found in {:.1} s: {e}", start.elapsed().as_secs_f32()))?;
        let value = found.read(&reader, &l, view, Instant::now()).map_err(|e| e.to_string())?;
        let from = if size.is_some() { "size" } else { "window" };
        println!("{}", serde_json::json!({ "locator": l.id, "version": l.version, "aspect": view, "aspect_from": from, "copies": found.len(), "seconds": start.elapsed().as_secs_f32(), "camera": camera(&value) }));
        Ok(())
    }

    /// One JSON line per second: valid reads, the last value or error. A map passes when at least
    /// 90% of the reads are valid (a view change can fail a few). Exit 1 otherwise.
    pub fn check(path: &str, pid: &str, seconds: Option<&str>, o: &Options) -> Result<(), String> {
        let size = o.size;
        let seconds: u64 = seconds.map_or(Ok(10), |s| s.parse().map_err(|_| format!("not a number of seconds: {s}")))?;
        let (pid, read): (u32, Read) = match package(path)? {
            Signed::Map(m) => { let (pid, r) = open(pid, &m.game, &m.module)?; (pid, exact(m, r)?) }
            Signed::Locator(l) => {
                let (pid, r) = open(pid, &l.game, &l.module)?;
                let mut found = l.find(&r, aspect(pid, size)?, &mut sleep).map_err(|e| e.to_string())?;
                println!("{}", serde_json::json!({ "locator": l.id, "copies": found.len() }));
                (pid, Box::new(move |w| match found.inspect(&r, &l, w, Instant::now()) {
                    (copies, Ok(v)) => Outcome { value: Ok(v), reason: "", copies, raw: serde_json::Value::Null },
                    (copies, Err(m)) => Outcome { value: Err(m.text().into()), reason: m.key(), copies, raw: serde_json::Value::Null },
                }))
            }
        };
        let trace = o.trace.map(|t| std::fs::File::create(t).map(std::io::BufWriter::new).map_err(|e| format!("{t}: {e}"))).transpose()?;
        run(pid, seconds, size, read, trace)
    }

    /// The camera symbol of an exact map (the first Projection), with the host's aspect check
    /// for a map without a locator and the same liveness proof.
    fn exact(m: Map, reader: Reader) -> Result<Read, String> {
        let id = reader.identity().map_err(|e| e.to_string())?;
        if let Some(exe) = m.sha256.as_ref().filter(|h| **h != id.sha256) { return Err(format!("the map is for exe {}, the game runs exe {}", exe, if id.sha256.is_empty() { "(unreadable file)" } else { &id.sha256 })); }
        let symbol = m.symbols.iter().find(|s| s.kind == fm_memory::Kind::Projection).ok_or("the map has no camera symbol")?;
        let key = format!("{}/{}", m.id, symbol.name);
        let image = reader.snapshot().map_err(|e| e.to_string())?;
        let runtime = Runtime::new(reader, vec![m.resolve(&image).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
        let mut live = Live::default();
        Ok(Box::new(move |w| {
            let first = runtime.read_raw(&key);
            match first.as_ref().map_err(|e| e.to_string()).and_then(|r| runtime.decode(&key, r).map_err(|e| e.to_string())) {
                Err(e) => Outcome { raw: failed(&runtime, &key, first.ok()), ..miss(e, "error") },
                Ok(Scalar::Camera { aspect, .. }) if (aspect / w - 1.0).abs() > 0.01 => miss(format!("the camera aspect {aspect:.4} is not the view aspect {w:.4}"), "gone"),
                Ok(v) if !live.see(&v, Instant::now()) => miss("the camera is not live (its frame jitter does not change)".into(), "stale"),
                Ok(v) => Outcome { value: Ok(v), reason: "", copies: vec![], raw: serde_json::json!({ "chain": first.as_ref().map(|r| r.chain.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>()).unwrap_or_default() }) },
            }
        }))
    }

    /// The record of a failed exact read for the trace: its pointer chain and value as floats,
    /// and a second read at once ("again"). These are observations: a changed chain points to a
    /// race on the pointers, and the floats show what the slot held (on 2026-10-21, other views
    /// with the same chain). Read the matrix contents before you name a cause.
    fn failed(runtime: &Runtime<Reader>, key: &str, raw: Option<fm_memory::RawRead>) -> serde_json::Value {
        let floats = |b: &[u8]| b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect::<Vec<_>>();
        let record = |r: &fm_memory::RawRead| serde_json::json!({ "chain": r.chain.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>(),
            "value": floats(&r.bytes), "ok": runtime.decode(key, r).is_ok() });
        let again = runtime.read_raw(key);
        serde_json::json!({ "first": raw.as_ref().map(record), "again": again.as_ref().ok().map(record) })
    }

    /// A kept copy in the trace: address, value (null when it fails the checks), jitter, proof.
    fn copy(c: &CopyRead) -> serde_json::Value {
        let jitter = match c.value { Some(Scalar::Camera { jitter, .. }) => serde_json::json!(jitter), _ => serde_json::Value::Null };
        serde_json::json!({ "address": format!("{:#x}", c.address), "camera": c.value.as_ref().map(camera), "jitter": jitter, "live": c.live })
    }

    /// Each second prints the valid reads and the misses by reason (gone, stale, tie, window,
    /// error); `trace` gets one line per read with each kept copy.
    fn run(pid: u32, seconds: u64, size: Option<f32>, mut read: Read, mut trace: Option<std::io::BufWriter<std::fs::File>>) -> Result<(), String> {
        let (mut good, mut all, start) = (0u64, 0u64, Instant::now());
        for second in 1..=seconds {
            let (mut last, mut ok, mut misses) = (serde_json::Value::Null, 0, std::collections::BTreeMap::<&str, u64>::new());
            let end = Instant::now() + Duration::from_secs(1);
            while Instant::now() < end {
                all += 1;
                let r = aspect(pid, size).map(&mut read).unwrap_or_else(|e| miss(e, "window"));
                match &r.value {
                    Ok(v) => { ok += 1; good += 1; last = camera(v); }
                    Err(e) => { last = serde_json::json!({ "error": e }); *misses.entry(r.reason).or_default() += 1; }
                }
                if let Some(t) = trace.as_mut() {
                    let line = serde_json::json!({ "ms": start.elapsed().as_millis() as u64, "ok": r.value.is_ok(), "reason": r.reason, "copies": r.copies.iter().map(copy).collect::<Vec<_>>(), "raw": r.raw });
                    writeln!(t, "{line}").map_err(|e| e.to_string())?;
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            println!("{}", serde_json::json!({ "second": second, "valid": ok, "misses": misses, "last": last }));
        }
        if let Some(t) = trace.as_mut() { t.flush().map_err(|e| e.to_string())?; }
        let share = good as f64 / all.max(1) as f64;
        println!("{}", serde_json::json!({ "reads": all, "valid": good, "pass": share >= 0.9 }));
        if share >= 0.9 { Ok(()) } else { Err(format!("only {:.0}% of the reads were valid", share * 100.0)) }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    // --size gives the aspect of the game's resolution, at any place after "find" or "check";
    // without it the tools use the window (None), as before.
    #[test]
    fn size_gives_the_game_aspect() {
        let (plain, o) = parse(&["check", "m.json", "42", "--size", "1920x1080", "30"]).unwrap();
        assert_eq!(plain, ["check", "m.json", "42", "30"]);
        assert!((o.size.unwrap() - 16.0 / 9.0).abs() < 1e-6);
        assert_eq!(parse(&["find", "m.json", "42"]).unwrap(), (vec!["find", "m.json", "42"], super::Options::default()));
    }

    // --trace names the file of the per-read record of `check`; `find` has no reads to record.
    #[test]
    fn trace_names_the_read_record() {
        let (plain, o) = parse(&["check", "m.json", "42", "--trace", "t.jsonl", "--size", "16x9"]).unwrap();
        assert_eq!((plain, o.trace), (vec!["check", "m.json", "42"], Some("t.jsonl")));
        assert!(parse(&["check", "m.json", "42", "--trace"]).is_err());
        assert!(parse(&["check", "--trace", "a", "m.json", "42", "--trace", "b"]).is_err());
        assert!(super::command(&["find", "m.json", "42", "--trace", "t.jsonl"]).unwrap_err().contains("usage"));
    }

    // A size that cannot be a resolution is refused, so a typing error never checks against a
    // wrong aspect.
    #[test]
    fn a_bad_size_is_refused() {
        for v in ["0x1080", "1920x0", "1920", "axb", "1920x1080x2", "-1920x1080", ""] {
            assert!(parse(&["find", "m.json", "42", "--size", v]).is_err(), "{v}");
        }
        assert!(parse(&["find", "m.json", "42", "--size"]).is_err());
        assert!(parse(&["find", "--size", "16x9", "m.json", "42", "--size", "16x9"]).is_err());
    }

    // map pin writes the digest of a changed map into its pin and keeps the rest of the text;
    // a second run changes nothing, and a map that fails the body rules is refused.
    #[test]
    fn pin_writes_the_map_digest() {
        let dir = tempfile::tempdir().unwrap();
        let map = serde_json::json!({ "kind": "memory-map", "contract": "camera-locator-v1", "locator": { "schema": 1, "id": "cam", "version": "1.0.0",
            "game": "msfs2024", "module": "FlightSimulator2024.exe", "near": [0.005, 2.0], "fov": [5.0, 150.0], "aspect_tolerance": 0.01 } });
        std::fs::create_dir_all(dir.path().join("maps/cam/1.0.0")).unwrap();
        std::fs::write(dir.path().join("maps/cam/1.0.0/map.json"), serde_json::to_vec(&map).unwrap()).unwrap();
        let manifest = format!("{{\n  \"id\": \"f\", \"name\": \"F\", \"version\": \"1.0.0\", \"description\": \"d\", \"api\": 5,\n  \"games\": [\"msfs2024\"], \"capabilities\": [\"render_pass\", \"render_depth\"],\n  \"memory_maps\": [{{ \"id\": \"cam\", \"version\": \"1.0.0\", \"sha256\": \"{}\" }}]\n}}\n", "0".repeat(64));
        std::fs::write(dir.path().join("feature.json"), &manifest).unwrap();
        assert_eq!(super::pin(dir.path()).unwrap(), 1);
        let sha = fm_install::memory::digest(&std::fs::read(dir.path().join("maps/cam/1.0.0/map.json")).unwrap());
        assert_eq!(std::fs::read_to_string(dir.path().join("feature.json")).unwrap(), manifest.replace(&"0".repeat(64), &sha));
        assert_eq!(super::pin(dir.path()).unwrap(), 0);
        std::fs::write(dir.path().join("maps/cam/1.0.0/map.json"), b"{}").unwrap();
        assert!(super::pin(dir.path()).is_err());
    }

    // Review round 2: two new maps with the same placeholder digest each get their own digest.
    // Round 3: an id of 64 zeroes (a valid id) equal to the placeholder stays unchanged.
    #[test]
    fn pin_writes_equal_placeholders_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut pins = Vec::new();
        let zeroes = "0".repeat(64);
        for (id, near) in [(zeroes.as_str(), 0.005), ("cam-b", 0.01)] {
            let map = serde_json::json!({ "kind": "memory-map", "contract": "camera-locator-v1", "locator": { "schema": 1, "id": id, "version": "1.0.0",
                "game": "msfs2024", "module": "FlightSimulator2024.exe", "near": [near, 2.0], "fov": [5.0, 150.0], "aspect_tolerance": 0.01 } });
            std::fs::create_dir_all(dir.path().join(format!("maps/{id}/1.0.0"))).unwrap();
            std::fs::write(dir.path().join(format!("maps/{id}/1.0.0/map.json")), serde_json::to_vec(&map).unwrap()).unwrap();
            pins.push(serde_json::json!({ "id": id, "version": "1.0.0", "sha256": "0".repeat(64) }));
        }
        let manifest = serde_json::json!({ "id": "f", "name": "F", "version": "1.0.0", "description": "d", "api": 5, "games": ["msfs2024"],
            "capabilities": ["render_pass", "render_depth"], "memory_maps": pins });
        std::fs::write(dir.path().join("feature.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        assert_eq!(super::pin(dir.path()).unwrap(), 2);
        let m: fm_install::feature::Manifest = serde_json::from_slice(&std::fs::read(dir.path().join("feature.json")).unwrap()).unwrap();
        assert_eq!(m.memory_maps[0].id, zeroes);
        for p in &m.memory_maps {
            assert_eq!(p.sha256, fm_install::memory::digest(&std::fs::read(dir.path().join(fm_install::memory::map_path(p))).unwrap()), "{}", p.id);
        }
        assert_eq!(super::pin(dir.path()).unwrap(), 0, "a second run changes nothing");
    }

    // The routes refuse extra or missing arguments with the usage text.
    #[test]
    fn the_routes_check_the_argument_count() {
        for args in [&["find", "m.json"][..], &["find", "m.json", "1", "2"], &["check", "m.json", "1", "2", "3"], &["pin"], &["pin", "d", "--size", "16x9"], &["list"]] {
            assert!(super::command(args).unwrap_err().contains("usage"), "{args:?}");
        }
    }

    // Live 2026-10-21: at full zoom MSFS 2024 gives vfov 1.146 degrees (tan of half 0.01), and the
    // 1.0.0 maps refused all below 5 degrees, so the mirage went off. Each shipped mirage map must
    // admit the full zoom range that MSFS gives (1.146 to the widest view), through the host checks.
    // The SDK repository (tools/sdk-export) has no features/ folder: there this check has nothing to do.
    #[test]
    fn the_mirage_maps_admit_the_full_msfs_zoom() {
        use fm_install::memory::{parse_package, Signed};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../features/mirage");
        if !root.parent().is_some_and(|f| f.is_dir()) { return; }
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("feature.json")).unwrap()).unwrap();
        let aspect = 3440.0 / 1417.0;
        let camera = |tan_half_v: f32| fm_memory::Scalar::Camera { near: 0.059, tan_half_v, aspect, jitter: [0.0; 2] };
        for pin in manifest["memory_maps"].as_array().unwrap() {
            let path = root.join(format!("maps/{}/{}/map.json", pin["id"].as_str().unwrap(), pin["version"].as_str().unwrap()));
            for tan_half_v in [0.01, 0.4467, 1.0] {
                let vfov = (2.0 * f64::from(tan_half_v).atan()).to_degrees();
                match parse_package(&std::fs::read(&path).unwrap()).unwrap() {
                    Signed::Map(m) => m.symbols.iter().for_each(|s| assert!((s.min..=s.max).contains(&vfov), "{} refuses {vfov}", m.id)),
                    Signed::Locator(l) => assert!(l.admits(&camera(tan_half_v), aspect), "{} refuses {vfov}", l.id),
                }
            }
        }
    }
}
