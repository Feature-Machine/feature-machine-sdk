// mirrored-setter-v2 (plan 2026-10-23-write-maps-survive-updates, slices 1 and 2): a map with no
// exe digest finds its object by name, proves its class (vtable in read-only data, enough named
// members, handler in code, setter state), then waits for the read-only watch before a writer.
// The fixture is a small PE image: .text, .rdata (vtables) and .data (tweak objects), with an
// optional header before the section table and a gap in the readable ranges, as in a snapshot.
use fm_memory::{Error, Found, Image, Map, Memory, ModuleId, Proof, Runtime, Scalar, WATCH_MS};

const BASE: u64 = 0x1_4000_0000;
const LEN: usize = 0x4000;
const VTABLE: usize = 0x2000;
/// A second tweak class in .rdata, for the shared-class rule.
const VTABLE2: usize = 0x2100;
const HANDLER: u64 = BASE + 0x1100;
/// The handler RVA that a saved result holds for the wet-speed setter.
const RVA: Option<u64> = Some(HANDLER - BASE);
/// Two other real functions in .text: WRONG clears the value, DTOR is slot 0 of the tweak
/// vtable. A slot that holds one of them is a wrong handler that is still in read-only code.
const WRONG: u64 = BASE + 0x1200;
const DTOR: u64 = BASE + 0x1300;
const OBJECTS: usize = 0x3000;
/// Pattern start inside an object; value, mirror and clamps are relative to it.
const P: usize = 8;
const VALUE: usize = 0x44;

/// `exe`: the exe digest that the reader gives ("" for a Store build that it cannot hash).
/// `writes`: the count of writes; `refuse_calls`: each handler call fails (and is not recorded).
struct Game { bytes: Vec<u8>, calls: Vec<(u64, u64)>, exe: String, writes: u32, refuse_calls: bool }

impl Game {
    /// `names[k]` is the inline name of object k (objects of 0x80 bytes in .data).
    fn new(names: &[&str]) -> Self {
        let mut b = vec![0u8; LEN];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        b[0x46..0x48].copy_from_slice(&3u16.to_le_bytes());
        b[0x54..0x56].copy_from_slice(&0xF0u16.to_le_bytes());
        for (i, (rva, flags)) in [(0x1000u32, 0x6000_0020u32), (0x2000, 0x4000_0040), (0x3000, 0xC000_0040)].iter().enumerate() {
            let o = 0x148 + 40 * i;
            b[o + 8..o + 12].copy_from_slice(&0x1000u32.to_le_bytes());
            b[o + 12..o + 16].copy_from_slice(&rva.to_le_bytes());
            b[o + 36..o + 40].copy_from_slice(&flags.to_le_bytes());
        }
        b[VTABLE + 16..VTABLE + 24].copy_from_slice(&HANDLER.to_le_bytes());
        b[VTABLE2 + 16..VTABLE2 + 24].copy_from_slice(&HANDLER.to_le_bytes());
        // x64 bodies (rcx = object): HANDLER copies the value (+0x4C) to the mirror (+0x48) like
        // slot 2 on 1.9.20.0; WRONG stores 0 in the value; DTOR loads the vtable pointer.
        let code: [(u64, &[u8]); 3] = [
            (HANDLER, &[0x40, 0x53, 0x48, 0x83, 0xEC, 0x20, 0x48, 0x8B, 0xD9, 0xF3, 0x0F, 0x10, 0x41, 0x4C, 0xF3, 0x0F, 0x11, 0x41, 0x48, 0x48, 0x83, 0xC4, 0x20, 0x5B, 0xC3]),
            (WRONG, &[0xC7, 0x41, 0x4C, 0x00, 0x00, 0x00, 0x00, 0xC3]),
            (DTOR, &[0x48, 0x83, 0xEC, 0x28, 0x48, 0x8B, 0x01, 0x48, 0x83, 0xC4, 0x28, 0xC3]),
        ];
        for (f, body) in code { let at = (f - BASE) as usize; b[at..at + body.len()].copy_from_slice(body); }
        b[VTABLE..VTABLE + 8].copy_from_slice(&DTOR.to_le_bytes());
        let mut g = Self { bytes: b, calls: vec![], exe: "c".repeat(64), writes: 0, refuse_calls: false };
        for (k, name) in names.iter().enumerate() { g.object(k, name, (BASE + VTABLE as u64).to_le_bytes(), 0.001); }
        g
    }
    fn object(&mut self, k: usize, name: &str, vtable: [u8; 8], value: f32) {
        let o = OBJECTS + k * 0x80;
        self.bytes[o..o + 8].copy_from_slice(&vtable);
        self.bytes[o + P] = 1;
        self.bytes[o + P + 8..o + P + 8 + name.len()].copy_from_slice(name.as_bytes());
        self.f32(o + P + VALUE - 4, value);
        self.f32(o + P + VALUE, value);
        self.f32(o + P + VALUE + 8, 0.0);
        self.f32(o + P + VALUE + 12, 1.0);
    }
    fn f32(&mut self, at: usize, v: f32) { self.bytes[at..at + 4].copy_from_slice(&v.to_le_bytes()); }
    /// Puts `function` in slot `n` of the tweak vtable (as an update or a hook could).
    fn slot(&mut self, n: usize, function: u64) { self.bytes[VTABLE + n * 8..VTABLE + n * 8 + 8].copy_from_slice(&function.to_le_bytes()); }
    fn value(&self, k: usize) -> (f32, f32) {
        let at = OBJECTS + k * 0x80 + P + VALUE;
        let f = |a: usize| f32::from_le_bytes(self.bytes[a..a + 4].try_into().unwrap());
        (f(at), f(at - 4))
    }
    fn image(&self, digest: &str) -> Image { Image { base: BASE, bytes: self.bytes.clone(), id: id(digest), readable: vec![0..0x2800, 0x2c00..LEN] } }
}

fn id(digest: &str) -> ModuleId { ModuleId { game: "msfs2024".into(), module: "test.exe".into(), sha256: digest.into(), session: 9 } }

impl Memory for Game {
    fn identity(&self) -> Result<ModuleId, Error> { Ok(id(&self.exe)) }
    fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> {
        let at = address.checked_sub(BASE).ok_or(Error::Unavailable("outside image".into()))? as usize;
        self.bytes.get(at..at + size).map(|b| b.to_vec()).ok_or(Error::Unavailable("outside image".into()))
    }
    fn can_write(&self) -> bool { true }
    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        let at = (address - BASE) as usize;
        self.bytes[at..at + data.len()].copy_from_slice(data);
        self.writes += 1;
        Ok(())
    }
    fn call(&mut self, function: u64, object: u64) -> Result<(), Error> {
        if self.refuse_calls { return Err(Error::Unavailable("call refused".into())); }
        self.calls.push((function, object));
        Ok(())
    }
}

fn hex(s: &str) -> String { s.bytes().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ") }

/// A v2 map for the object named `name`; `members` is the class population that it needs.
fn map(name: &str, members: u32, digest: Option<&str>) -> Map {
    let mut m = serde_json::json!({
        "schema": 1, "id": "weather", "version": "1.1.0", "game": "msfs2024", "module": "test.exe",
        "symbols": [{ "name": "wet-speed", "pattern": format!("01 00 00 00 00 00 00 00 {} 00", hex(name)), "offset": VALUE, "kind": "f32",
            "access": "write", "min": 0.0, "max": 1.0,
            "class_guard": { "offset": -8, "member": format!("01 00 00 00 00 00 00 00 {}", hex("GAME/")), "members": members },
            "setter": { "contract": "mirrored_notify_v1", "mirror": -4, "clamp_min": 8, "clamp_max": 12, "notify_slot": 2 } }]
    });
    if let Some(d) = digest { m["sha256"] = d.into(); }
    serde_json::from_value(m).unwrap()
}

const NAMES: [&str; 6] = ["GAME/Weather/WetSpeed", "GAME/A", "GAME/B", "GAME/C", "GAME/D", "GAME/E"];
const KEY: &str = "weather/wet-speed";

fn runtime(g: Game, m: &Map) -> Runtime<Game> {
    let resolved = m.resolve(&g.image(&"c".repeat(64))).unwrap();
    Runtime::new(g, vec![resolved]).unwrap()
}

/// Five watch reads 2.5 s apart: the last is at WATCH_MS.
fn watch_full(r: &mut Runtime<Game>) -> Vec<String> {
    (0..5).flat_map(|i| r.watch(1_000 + i * WATCH_MS / 4)).collect()
}

#[test]
fn a_map_without_a_digest_writes_only_after_the_watch() {
    let m = map("GAME/Weather/WetSpeed", 4, None);
    let mut r = runtime(Game::new(&NAMES), &m);
    assert_eq!(r.acquire("f", KEY), Err(Error::Busy), "no writer before the watch: Busy, so the feature tries again");
    assert_eq!(r.read(KEY), Ok(Scalar::F32(0.001)), "reads work during the watch");
    assert_eq!(watch_full(&mut r), vec![KEY.to_string()]);
    let h = r.acquire("f", KEY).unwrap();
    r.set("f", h, Scalar::F32(0.5)).unwrap();
    assert_eq!(r.memory_mut().value(0), (0.5, 0.5));
    assert_eq!(r.memory_mut().calls, vec![(HANDLER, BASE + OBJECTS as u64)], "handler from the proved vtable, object at its vtable pointer");
    r.release("f", h).unwrap();
    assert_eq!(r.memory_mut().value(0), (0.001, 0.001));
}

#[test]
fn the_watch_needs_both_time_and_reads() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    assert!(r.watch(0).is_empty() && r.watch(WATCH_MS).is_empty(), "two reads over 10 s are not enough");
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    assert!((0..10).all(|i| r.watch(i * 100).is_empty()), "ten reads in 1 s are not enough");
}

#[test]
fn a_failed_watch_read_starts_the_watch_again() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    r.watch(0);
    r.watch(2_500);
    let at = OBJECTS + P + VALUE - 4;
    r.memory_mut().f32(at, 0.7); // mirror no longer equals the value
    assert!(r.watch(5_000).is_empty());
    r.memory_mut().f32(at, 0.001);
    assert!(r.watch(7_500).is_empty() && r.watch(WATCH_MS).is_empty(), "the watch started again at 7.5 s");
    assert!(!r.proved(KEY));
}

#[test]
fn another_build_digest_does_not_matter_but_the_game_and_module_do() {
    let g = Game::new(&NAMES);
    let m = map("GAME/Weather/WetSpeed", 4, None);
    assert!(m.resolve(&g.image(&"d".repeat(64))).is_ok());
    let mut other = g.image(&"d".repeat(64));
    other.id.module = "other.exe".into();
    assert!(matches!(m.resolve(&other), Err(Error::Unavailable(_))));
}

#[test]
fn too_few_class_members_refuse_the_object() {
    let g = Game::new(&NAMES[..3]);
    let e = map("GAME/Weather/WetSpeed", 4, None).resolve(&g.image("x")).err().unwrap();
    assert!(matches!(&e, Error::Unavailable(t) if t.contains("3 members, 4 needed")), "{e:?}");
}

#[test]
fn a_vtable_outside_read_only_data_refuses_the_object() {
    for vtable in [BASE + 0x3800, BASE + 0x1200, BASE + 0x9000] {
        let mut g = Game::new(&NAMES);
        for k in 0..NAMES.len() { g.bytes[OBJECTS + k * 0x80..OBJECTS + k * 0x80 + 8].copy_from_slice(&vtable.to_le_bytes()); }
        let e = map("GAME/Weather/WetSpeed", 4, None).resolve(&g.image("x")).err().unwrap();
        assert!(matches!(&e, Error::Unavailable(t) if t.contains("object class differs")), "{vtable:#x}: {e:?}");
    }
}

#[test]
fn a_handler_outside_code_refuses_the_setter() {
    let mut g = Game::new(&NAMES);
    g.bytes[VTABLE + 16..VTABLE + 24].copy_from_slice(&(BASE + 0x2800).to_le_bytes());
    let e = map("GAME/Weather/WetSpeed", 4, None).resolve(&g.image("x")).err().unwrap();
    assert_eq!(e, Error::Unavailable("change handler outside module code".into()));
}

#[test]
fn a_setter_state_against_the_contract_refuses_the_object() {
    let mut g = Game::new(&NAMES);
    g.f32(OBJECTS + P + VALUE - 4, 0.5); // mirror
    assert!(matches!(map("GAME/Weather/WetSpeed", 4, None).resolve(&g.image("x")), Err(Error::Unavailable(t)) if t.contains("mirror")));
    let mut g = Game::new(&NAMES);
    g.f32(OBJECTS + P + VALUE + 12, 0.0005); // native max below the value
    assert!(matches!(map("GAME/Weather/WetSpeed", 4, None).resolve(&g.image("x")), Err(Error::Unavailable(t)) if t.contains("clamp")));
}

#[test]
fn a_second_object_with_the_name_is_ambiguous() {
    let g = Game::new(&["GAME/Weather/WetSpeed", "GAME/Weather/WetSpeed", "GAME/B", "GAME/C", "GAME/D"]);
    assert!(matches!(map("GAME/Weather/WetSpeed", 4, None).resolve(&g.image("x")), Err(Error::Unavailable(t)) if t.contains("ambiguous")));
}

#[test]
fn a_changed_vtable_after_the_proof_refuses_the_writer() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    watch_full(&mut r);
    r.memory_mut().bytes[OBJECTS..OBJECTS + 8].copy_from_slice(&(BASE + 0x2100).to_le_bytes());
    assert!(matches!(r.acquire("f", KEY), Err(Error::Unavailable(_))));
}

#[test]
fn map_rules_for_the_class_guard() {
    let mut m = serde_json::to_value(map("GAME/Weather/WetSpeed", 4, None)).unwrap();
    let mut no_guard = m.clone();
    no_guard["symbols"][0].as_object_mut().unwrap().remove("class_guard");
    assert!(serde_json::from_value::<Map>(no_guard).unwrap().check().is_err(), "no digest and no class guard");
    m["symbols"][0]["type_guard"] = serde_json::json!({ "offset": -8, "rva": VTABLE });
    assert!(serde_json::from_value::<Map>(m.clone()).unwrap().check().is_err(), "a type guard and a class guard");
    let mut odd = serde_json::to_value(map("GAME/Weather/WetSpeed", 4, None)).unwrap();
    odd["symbols"][0]["class_guard"]["offset"] = (-4).into();
    assert!(serde_json::from_value::<Map>(odd).unwrap().check().is_err(), "an unaligned pointer place");
    let mut one = serde_json::to_value(map("GAME/Weather/WetSpeed", 1, None)).unwrap();
    one["symbols"][0]["class_guard"]["members"] = 1.into();
    assert!(serde_json::from_value::<Map>(one).unwrap().check().is_err(), "a class of one is no proof");
}

#[test]
fn an_exact_map_still_needs_its_digest_and_writes_at_once() {
    let g = Game::new(&NAMES);
    let mut m = serde_json::to_value(map("GAME/Weather/WetSpeed", 4, Some(&"c".repeat(64)))).unwrap();
    m["symbols"][0].as_object_mut().unwrap().remove("class_guard");
    m["symbols"][0]["type_guard"] = serde_json::json!({ "offset": -8, "rva": VTABLE });
    let m: Map = serde_json::from_value(m).unwrap();
    assert!(matches!(m.resolve(&g.image(&"d".repeat(64))), Err(Error::Unavailable(t)) if t == "module identity differs from the map"));
    let mut r = runtime(g, &m);
    assert!(r.proved(KEY) && r.acquire("f", KEY).is_ok(), "v1 has no watch");
}

/// A map with `wet-speed` and `dry-speed`, both in the tweak class (review r1: one class).
fn two(members: u32) -> Map {
    let one = |name: &str, setting: &str| serde_json::json!({ "name": name, "pattern": format!("01 00 00 00 00 00 00 00 {} 00", hex(setting)), "offset": VALUE,
        "kind": "f32", "access": "write", "min": 0.0, "max": 1.0,
        "class_guard": { "offset": -8, "member": format!("01 00 00 00 00 00 00 00 {}", hex("GAME/")), "members": members },
        "setter": { "contract": "mirrored_notify_v1", "mirror": -4, "clamp_min": 8, "clamp_max": 12, "notify_slot": 2 } });
    serde_json::from_value(serde_json::json!({ "schema": 1, "id": "weather", "version": "1.1.0", "game": "msfs2024", "module": "test.exe",
        "symbols": [one("wet-speed", "GAME/Weather/WetSpeed"), one("dry-speed", "GAME/Weather/DrySpeed")] })).unwrap()
}

#[test]
fn symbols_of_one_class_must_share_its_vtable() {
    let names = ["GAME/Weather/WetSpeed", "GAME/Weather/DrySpeed", "GAME/A", "GAME/B", "GAME/C", "GAME/D"];
    let g = Game::new(&names);
    assert!(two(4).resolve(&g.image("x")).is_ok(), "both settings in the tweak class");
    // DrySpeed moves to another class that alone passes the population check (5 members).
    let mut g = Game::new(&names);
    let other = (BASE + VTABLE2 as u64).to_le_bytes();
    g.object(1, "GAME/Weather/DrySpeed", other, 0.001);
    for k in 6..10 { g.object(k, &format!("GAME/X{k}"), other, 0.5); }
    let e = two(4).resolve(&g.image("x")).err().unwrap();
    assert!(matches!(&e, Error::Unavailable(t) if t.contains("symbols of one class have other vtables")), "{e:?}");
}

/// The saved result of the proved one-symbol map on exe "c..c" with an all-zero file digest.
fn saved() -> Proof {
    Proof { exe: "c".repeat(64), map: "weather".into(), version: "1.1.0".into(), file: "0".repeat(64),
        symbols: [("wet-speed".to_string(), Found { pattern: (OBJECTS + P) as u64, vtable: VTABLE as u64, handler: RVA })].into() }
}

#[test]
fn a_proved_map_gives_its_saved_result_and_an_exact_map_none() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    assert_eq!(r.proof("weather"), None, "no saved result before the watch");
    assert!(r.unproved("weather"));
    watch_full(&mut r);
    assert_eq!(r.proof("weather"), Some(saved()));
    assert!(!r.unproved("weather") && r.proof("other").is_none());
    let mut m = serde_json::to_value(map("GAME/Weather/WetSpeed", 4, Some(&"c".repeat(64)))).unwrap();
    m["symbols"][0].as_object_mut().unwrap().remove("class_guard");
    m["symbols"][0]["type_guard"] = serde_json::json!({ "offset": -8, "rva": VTABLE });
    let r = runtime(Game::new(&NAMES), &serde_json::from_value(m).unwrap());
    assert_eq!(r.proof("weather"), None, "an exact map needs no saved result");
}

#[test]
fn a_saved_result_skips_the_search_and_the_watch_on_its_exe() {
    let g = Game::new(&NAMES);
    let m = map("GAME/Weather/WetSpeed", 4, None);
    let resolved = m.resolve_proved(&g.image(&"c".repeat(64)), [0; 32], &saved()).unwrap();
    let mut r = Runtime::new(g, vec![resolved]).unwrap();
    assert!(r.proved(KEY) && !r.unproved("weather"), "no watch");
    let h = r.acquire("f", KEY).unwrap();
    r.set("f", h, Scalar::F32(0.5)).unwrap();
    assert_eq!(r.memory_mut().calls, vec![(HANDLER, BASE + OBJECTS as u64)]);
    // The class scan runs again (review s3 r1): fewer members at this start refuse the record.
    let e = map("GAME/Weather/WetSpeed", 4, None).resolve_proved(&Game::new(&NAMES[..2]).image(&"c".repeat(64)), [0; 32], &saved()).err().unwrap();
    assert!(matches!(&e, Error::Unavailable(t) if t.contains("2 members, 4 needed")), "{e:?}");
}

#[test]
fn an_exe_with_no_digest_never_saves_or_uses_a_result() {
    let mut g = Game::new(&NAMES);
    g.exe = String::new();
    let m = map("GAME/Weather/WetSpeed", 4, None);
    let mut r = Runtime::new(g, vec![m.resolve(&Game::new(&NAMES).image("")).unwrap()]).unwrap();
    watch_full(&mut r);
    assert!(r.proved(KEY), "the watch still proves it");
    assert_eq!(r.proof("weather"), None, "no result saved without an exe digest");
    let empty = Proof { exe: String::new(), ..saved() };
    assert!(matches!(m.resolve_proved(&Game::new(&NAMES).image(""), [0; 32], &empty), Err(Error::Unavailable(_))), "no result used without an exe digest");
}

#[test]
fn each_saved_vtable_of_a_shared_class_is_compared() {
    let names = ["GAME/Weather/WetSpeed", "GAME/Weather/DrySpeed", "GAME/A", "GAME/B", "GAME/C", "GAME/D"];
    let g = Game::new(&names);
    let found = |k: usize, vtable: usize| Found { pattern: (OBJECTS + k * 0x80 + P) as u64, vtable: vtable as u64, handler: RVA };
    let proof = |dry: usize| Proof { exe: "c".repeat(64), map: "weather".into(), version: "1.1.0".into(), file: "0".repeat(64),
        symbols: [("wet-speed".to_string(), found(0, VTABLE)), ("dry-speed".to_string(), found(1, dry))].into() };
    let image = g.image(&"c".repeat(64));
    assert!(two(4).resolve_proved(&image, [0; 32], &proof(VTABLE)).is_ok());
    let e = two(4).resolve_proved(&image, [0; 32], &proof(VTABLE2)).err().unwrap();
    assert!(matches!(&e, Error::Unavailable(t) if t.contains("saved result differs from the image: vtable")), "{e:?}");
}

#[test]
fn a_watch_that_fails_gives_its_reason() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    r.watch(0);
    assert_eq!(r.watch_error("weather"), None, "the reads pass");
    r.memory_mut().f32(OBJECTS + P + VALUE - 4, 0.7);
    r.watch(1_000);
    assert!(r.watch_error("weather").is_some_and(|e| e.starts_with("wet-speed: ") && e.contains("mirror")), "{:?}", r.watch_error("weather"));
    r.memory_mut().f32(OBJECTS + P + VALUE - 4, 0.001);
    watch_full(&mut r);
    assert_eq!(r.watch_error("weather"), None, "a proved map has no wait reason");
}

#[test]
fn a_saved_result_that_differs_is_refused() {
    let m = map("GAME/Weather/WetSpeed", 4, None);
    let g = Game::new(&NAMES);
    let image = g.image(&"c".repeat(64));
    let refused = |p: &Proof, image: &Image, digest: [u8; 32]| matches!(m.resolve_proved(image, digest, p), Err(Error::Unavailable(_)));
    assert!(refused(&saved(), &g.image(&"d".repeat(64)), [0; 32]), "another exe");
    assert!(refused(&saved(), &image, [1; 32]), "another map file");
    assert!(refused(&Proof { version: "1.0.0".into(), ..saved() }, &image, [0; 32]), "another map version");
    assert!(refused(&Proof { symbols: Default::default(), ..saved() }, &image, [0; 32]), "other symbols");
    let moved = |f: Found| Proof { symbols: [("wet-speed".to_string(), f)].into(), ..saved() };
    assert!(refused(&moved(Found { pattern: (OBJECTS + 0x80 + P) as u64, vtable: VTABLE as u64, handler: RVA }), &image, [0; 32]), "the pattern is not at the saved place");
    assert!(refused(&moved(Found { pattern: u64::MAX, vtable: VTABLE as u64, handler: RVA }), &image, [0; 32]), "a saved place outside the image");
    assert!(refused(&moved(Found { pattern: (OBJECTS + P) as u64, vtable: VTABLE2 as u64, handler: RVA }), &image, [0; 32]), "another vtable");
    let handler = |handler: Option<u64>| moved(Found { pattern: (OBJECTS + P) as u64, vtable: VTABLE as u64, handler });
    assert!(refused(&handler(Some(WRONG - BASE)), &image, [0; 32]), "another handler RVA");
    assert!(refused(&handler(None), &image, [0; 32]), "a record with no handler (older format)");
    // The checks of a full resolve still run: a mirror against the contract.
    let mut bad = Game::new(&NAMES);
    bad.f32(OBJECTS + P + VALUE - 4, 0.5);
    assert!(refused(&saved(), &bad.image(&"c".repeat(64)), [0; 32]), "setter state");
    let mut exact = m.clone();
    exact.sha256 = Some("c".repeat(64));
    assert!(matches!(exact.resolve_proved(&image, [0; 32], &saved()), Err(Error::Unavailable(_))), "an exact map takes no saved result");
}

// Wrong executable handler (GPT-6-Astra review, overnight 2026-10-26): slot 2 holds ANOTHER
// function in read-only code. Handler slot stability (option 2): the record saves the handler
// RVA and the runtime reads the slot again before each use, so a CHANGE is refused. `limit_`
// tests lock what stays: a first resolution takes a stable wrong function. The fake game only
// records calls, never runs native code. Identity check: the owner decides. Never test in the game.

/// The tweak object: the `object` argument of each handler call.
const OBJECT: u64 = BASE + OBJECTS as u64;

/// Acquire, write 0.5 and release; gives the handler calls (function, object).
fn write_once(r: &mut Runtime<Game>) -> Vec<(u64, u64)> {
    let h = r.acquire("f", KEY).unwrap();
    r.set("f", h, Scalar::F32(0.5)).unwrap();
    r.release("f", h).unwrap();
    std::mem::take(&mut r.memory_mut().calls)
}

#[test]
fn limit_a_wrong_function_in_code_passes_the_initial_resolution_and_the_watch() {
    let m = map("GAME/Weather/WetSpeed", 4, None);
    // HANDLER + 6 skips the prologue: never safe to run; safe here only because calls are recorded.
    for (why, wrong) in [("another function", WRONG), ("the slot 0 function", DTOR), ("the middle of the right function", HANDLER + 6)] {
        let mut g = Game::new(&NAMES);
        g.slot(2, wrong);
        let mut r = runtime(g, &m);
        assert_eq!(watch_full(&mut r), vec![KEY.to_string()], "{why}: the watch does not read the handler");
        assert_eq!(write_once(&mut r), vec![(wrong, OBJECT), (wrong, OBJECT)], "{why}: LIMIT, the write and the restore call it");
    }
}

#[test]
fn limit_an_exact_map_also_takes_a_wrong_function_in_code() {
    let mut g = Game::new(&NAMES);
    g.slot(2, WRONG);
    let mut m = serde_json::to_value(map("GAME/Weather/WetSpeed", 4, Some(&"c".repeat(64)))).unwrap();
    m["symbols"][0].as_object_mut().unwrap().remove("class_guard");
    m["symbols"][0]["type_guard"] = serde_json::json!({ "offset": -8, "rva": VTABLE });
    let mut r = runtime(g, &serde_json::from_value(m).unwrap());
    // v1 pins the exe digest: if the approved slot was right for that exe, only a live patch before
    // the snapshot gets here. The digest proves the file, not what the handler does.
    assert_eq!(write_once(&mut r), vec![(WRONG, OBJECT), (WRONG, OBJECT)], "LIMIT: no watch, the wrong function is called");
}

#[test]
fn refused_a_handler_in_writable_code_or_in_data() {
    let m = map("GAME/Weather/WetSpeed", 4, None);
    let mut g = Game::new(&NAMES);
    g.bytes[0x148 + 36..0x148 + 40].copy_from_slice(&0xE000_0020u32.to_le_bytes()); // .text gets write
    assert_eq!(m.resolve(&g.image("x")).err(), Some(Error::Unavailable("change handler outside module code".into())));
    // The saved path checks the handler again (plan slice 3: "the handler in code").
    let mut g = Game::new(&NAMES);
    g.slot(2, BASE + VTABLE2 as u64);
    let e = m.resolve_proved(&g.image(&"c".repeat(64)), [0; 32], &saved()).err();
    assert_eq!(e, Some(Error::Unavailable("change handler outside module code".into())));
}

#[test]
fn refused_a_saved_result_whose_handler_changed_on_its_exe() {
    let m = map("GAME/Weather/WetSpeed", 4, None);
    let mut r = runtime(Game::new(&NAMES), &m);
    watch_full(&mut r);
    assert_eq!(write_once(&mut r), vec![(HANDLER, OBJECT), (HANDLER, OBJECT)], "proved with the right handler");
    let proof = r.proof("weather").unwrap();
    assert_eq!(proof, saved(), "the record holds the pattern, vtable and handler RVAs");
    // The next start on the same exe digest: slot 2 now holds another function (a hook).
    let mut g = Game::new(&NAMES);
    g.slot(2, WRONG);
    let e = m.resolve_proved(&g.image(&"c".repeat(64)), [0; 32], &proof).err();
    assert_eq!(e, Some(Error::Unavailable("wet-speed: saved result differs from the image: handler".into())));
    // Residual: the host fallback is the full search, which takes a stable wrong function.
    let mut r = runtime(g, &m);
    assert_eq!(watch_full(&mut r), vec![KEY.to_string()], "LIMIT: the full search and the watch take it");
}

#[test]
fn an_old_record_without_the_handler_reads_but_is_refused() {
    let old = serde_json::json!({ "exe": "c".repeat(64), "map": "weather", "version": "1.1.0", "file": "0".repeat(64),
        "symbols": { "wet-speed": { "pattern": OBJECTS + P, "vtable": VTABLE } } });
    let old: Proof = serde_json::from_value(old).unwrap();
    assert_eq!(old.symbols["wet-speed"].handler, None);
    let e = map("GAME/Weather/WetSpeed", 4, None).resolve_proved(&Game::new(&NAMES).image(&"c".repeat(64)), [0; 32], &old).err();
    assert_eq!(e, Some(Error::Unavailable("wet-speed: saved result differs from the image: handler".into())));
    let new = serde_json::to_value(saved()).unwrap();
    assert_eq!(new["symbols"]["wet-speed"]["handler"], serde_json::json!(HANDLER - BASE), "a new record writes the field");
}

#[test]
fn refused_a_slot_change_during_the_watch() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    r.memory_mut().slot(2, WRONG); // live memory, after the image snapshot
    assert!(watch_full(&mut r).is_empty() && !r.proved(KEY), "the watch reads the slot each time");
    assert!(r.watch_error("weather").is_some_and(|e| e.contains("change handler slot differs")), "{:?}", r.watch_error("weather"));
    assert!(matches!(r.acquire("f", KEY), Err(Error::Unavailable(_))), "no writer");
    // The slot back to the resolved handler: a new full watch proves it. No call before that.
    r.memory_mut().slot(2, HANDLER);
    assert_eq!((0..5).flat_map(|i| r.watch(20_000 + i * WATCH_MS / 4)).collect::<Vec<_>>(), vec![KEY.to_string()]);
    assert!(r.memory_mut().calls.is_empty());
}

#[test]
fn refused_a_write_after_a_slot_change_between_acquire_and_set() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    watch_full(&mut r);
    let h = r.acquire("f", KEY).unwrap();
    r.memory_mut().slot(2, DTOR);
    assert_eq!(r.set("f", h, Scalar::F32(0.5)), Err(Error::Unavailable("change handler slot differs from the resolved handler".into())));
    assert_eq!((r.memory_mut().calls.len(), r.memory_mut().value(0)), (0, (0.001, 0.001)), "no write, no call");
    // Nothing to restore: the lease ends with no write and no call, as its object guard holds.
    assert_eq!(r.release("f", h), Ok(()));
    assert!(!r.holds("f") && r.memory_mut().calls.is_empty());
    assert!(matches!(r.acquire("f", KEY), Err(Error::Unavailable(_))), "a new writer is refused at acquire");
}

#[test]
fn refused_a_pending_restore_call_after_a_slot_change_then_one_call_with_no_write() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    watch_full(&mut r);
    let h = r.acquire("f", KEY).unwrap();
    r.set("f", h, Scalar::F32(0.5)).unwrap();
    // The restore writes the original, then its handler call fails: the call stays pending.
    r.memory_mut().refuse_calls = true;
    assert!(r.release("f", h).is_err() && r.holds("f"));
    let writes = r.memory_mut().writes;
    assert_eq!((r.memory_mut().value(0), r.memory_mut().calls.len()), ((0.001, 0.001), 1), "restored, only the set call");
    // The slot changes while the call is pending: refused, no call, no write, the lease stays.
    r.memory_mut().refuse_calls = false;
    r.memory_mut().slot(2, WRONG);
    assert!(matches!(r.release("f", h), Err(Error::Unavailable(t)) if t.contains("change handler slot differs")));
    assert_eq!((r.holds("f"), r.memory_mut().calls.len(), r.memory_mut().writes), (true, 1, writes));
    // The slot back: one call of the resolved handler, no other write, and the lease ends.
    r.memory_mut().slot(2, HANDLER);
    assert_eq!(r.release("f", h), Ok(()));
    assert_eq!((r.holds("f"), r.memory_mut().calls.clone(), r.memory_mut().writes), (false, vec![(HANDLER, OBJECT); 2], writes));
}

#[test]
fn refused_a_restore_after_a_slot_change_keeps_the_lease_and_the_written_value() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    watch_full(&mut r);
    let h = r.acquire("f", KEY).unwrap();
    r.set("f", h, Scalar::F32(0.5)).unwrap();
    r.memory_mut().slot(2, WRONG);
    assert!(matches!(r.release("f", h), Err(Error::Unavailable(t)) if t.contains("change handler slot differs")));
    // As for a class change: no write, no call, the lease stays. The game keeps the value and
    // mirror of its last handler call, a state that its own handler made.
    assert!(r.holds("f"), "the lease stays for a later cleanup");
    assert_eq!((r.memory_mut().value(0), r.memory_mut().calls.clone()), ((0.5, 0.5), vec![(HANDLER, OBJECT)]), "no write, no call");
    assert_eq!((r.stop("f").len(), r.holds("f"), r.admit("f")), (1, true, Err(Error::Refused)), "stop reports it; the owner stays closed");
    // The slot back (a hook removed): the later cleanup restores and calls the resolved handler.
    r.memory_mut().slot(2, HANDLER);
    assert_eq!(r.release("f", h), Ok(()));
    assert_eq!((r.memory_mut().value(0), r.memory_mut().calls.clone()), ((0.001, 0.001), vec![(HANDLER, OBJECT); 2]));
}

#[test]
fn refused_an_object_that_moves_to_a_class_with_another_handler_during_a_lease() {
    let mut r = runtime(Game::new(&NAMES), &map("GAME/Weather/WetSpeed", 4, None));
    watch_full(&mut r);
    let h = r.acquire("f", KEY).unwrap();
    r.memory_mut().bytes[VTABLE2 + 16..VTABLE2 + 24].copy_from_slice(&WRONG.to_le_bytes());
    r.memory_mut().bytes[OBJECTS..OBJECTS + 8].copy_from_slice(&(BASE + VTABLE2 as u64).to_le_bytes());
    assert!(matches!(r.set("f", h, Scalar::F32(0.5)), Err(Error::Unavailable(_))), "the object guard sees the vtable pointer");
    assert!(matches!(r.release("f", h), Err(Error::Unavailable(_))) && r.holds("f"), "the lease stays for a later cleanup");
    assert_eq!((r.memory_mut().calls.len(), r.memory_mut().value(0)), (0, (0.001, 0.001)), "no write, no call");
}
