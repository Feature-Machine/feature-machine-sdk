// Contract 2 of the session sync plan (2026-10-19, section 12): an owner opens again after a
// stop, and a running runtime takes more maps with the same checks as `Runtime::new`. All
// through the public runtime, with a fixture that has one writer, its guard and free space.
// One byte range per readable list is intended in these fixtures.
#![allow(clippy::single_range_in_vec_init)]
use fm_memory::{Added, Error, Image, Map, Memory, ModuleId, ResolvedMap, Runtime, Scalar};

struct Fixture { bytes: Vec<u8>, execution: bool }

impl Memory for Fixture {
    fn identity(&self) -> Result<ModuleId, Error> { Ok(identity(17)) }
    fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> {
        self.bytes.get(address as usize..address as usize + size).map(|b| b.to_vec()).ok_or(Error::Unavailable("outside image".into()))
    }
    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        if !self.execution { return Err(Error::Unavailable("no execution point".into())); }
        self.bytes[address as usize..address as usize + data.len()].copy_from_slice(data);
        Ok(())
    }
    fn can_write(&self) -> bool { self.execution }
}

fn identity(session: u64) -> ModuleId { ModuleId { game: "msfs2024".into(), module: "test.exe".into(), sha256: "a".repeat(64), session } }

/// "FM_VALUE" at 8 guards the writer at 16..20; "FM_OTHER" at 20 is free for other maps.
fn bytes() -> Vec<u8> {
    let mut b = vec![0; 64];
    b[8..16].copy_from_slice(b"FM_VALUE");
    b[16..20].copy_from_slice(&2.5f32.to_le_bytes());
    b[20..28].copy_from_slice(b"FM_OTHER");
    b
}

fn image(session: u64) -> Image { Image { base: 0, bytes: bytes(), id: identity(session), readable: vec![0..64] } }

fn map(id: &str, version: &str, pattern: &str, offset: i64, access: &str) -> Map {
    serde_json::from_value(serde_json::json!({
        "schema": 1, "id": id, "version": version, "game": "msfs2024", "module": "test.exe", "sha256": "a".repeat(64),
        "symbols": [{ "name": "scalar", "pattern": pattern, "offset": offset, "kind": "f32", "access": access, "min": 0.0, "max": 10.0 }]
    })).unwrap()
}

const VALUE: &str = "46 4D 5F 56 41 4C 55 45";
/// A guard from 16 to 28: its first four bytes are the writer's value.
const OVER_WRITER: &str = "?? ?? ?? ?? 46 4D 5F 4F 54 48 45 52";

fn writer() -> Map { map("writer", "1.0.0", VALUE, 8, "write") }
fn resolved(m: &Map, digest: u8) -> ResolvedMap { m.resolve_pinned(&image(17), [digest; 32]).unwrap() }
fn runtime(maps: Vec<ResolvedMap>) -> Runtime<Fixture> { Runtime::new(Fixture { bytes: bytes(), execution: true }, maps).unwrap() }
fn added(new: &[&str], present: &[&str]) -> Added { Added { new: new.iter().map(|s| s.to_string()).collect(), present: present.iter().map(|s| s.to_string()).collect() } }

#[test]
fn a_stopped_owner_opens_again_and_its_old_handle_stays_refused() {
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    let old = r.acquire("f", "writer/scalar").unwrap();
    r.set("f", old, Scalar::F32(4.0)).unwrap();
    assert!(r.stop("f").is_empty());
    assert_eq!(r.read("writer/scalar"), Ok(Scalar::F32(2.5)), "the stop restored the value");
    assert_eq!(r.acquire("f", "writer/scalar"), Err(Error::Refused));
    assert_eq!(r.admit("f"), Ok(()));
    let new = r.acquire("f", "writer/scalar").unwrap();
    assert_ne!(new, old);
    assert_eq!(r.set("f", old, Scalar::F32(5.0)), Err(Error::Refused), "the handle from before the stop");
    r.set("f", new, Scalar::F32(6.0)).unwrap();
    assert_eq!(r.read("writer/scalar"), Ok(Scalar::F32(6.0)));
    assert!(r.stop("f").is_empty());
    assert_eq!(r.read("writer/scalar"), Ok(Scalar::F32(2.5)));
}

#[test]
fn a_failed_restore_blocks_admission_until_it_succeeds() {
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    let h = r.acquire("f", "writer/scalar").unwrap();
    r.set("f", h, Scalar::F32(4.0)).unwrap();
    r.memory_mut().execution = false;
    assert!(!r.stop("f").is_empty(), "the restore fails outside the execution point");
    assert!(r.holds("f"));
    assert_eq!(r.admit("f"), Err(Error::Refused));
    r.memory_mut().execution = true;
    assert!(r.stop("f").is_empty(), "a retry restores");
    assert_eq!(r.read("writer/scalar"), Ok(Scalar::F32(2.5)));
    assert_eq!(r.admit("f"), Ok(()));
    assert!(r.acquire("f", "writer/scalar").is_ok());
}

#[test]
fn admission_after_disconnect_is_refused() {
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    r.stop("f");
    assert!(r.disconnect().is_empty());
    assert_eq!(r.admit("f"), Err(Error::Refused), "disconnect is final for the runtime");
    assert_eq!(r.admit(""), Err(Error::Refused));
}

#[test]
fn maps_added_during_a_held_write_keep_its_lease_and_restore() {
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    let h = r.acquire("first", "writer/scalar").unwrap();
    r.set("first", h, Scalar::F32(4.0)).unwrap();
    // A read alias of the held value is legal, as in `new`; a second writer of it gets Busy.
    let alias = map("alias", "1.0.0", VALUE, 8, "read");
    assert_eq!(r.add_maps(vec![resolved(&alias, 2)]), Ok(added(&["alias"], &[])));
    assert_eq!(r.read("alias/scalar"), Ok(Scalar::F32(4.0)));
    assert_eq!(r.add_maps(vec![resolved(&alias, 2), resolved(&writer(), 1)]), Ok(added(&[], &["alias", "writer"])), "same identity");
    r.release("first", h).unwrap();
    assert_eq!(r.read("writer/scalar"), Ok(Scalar::F32(2.5)), "the restore did not change");
    let second = map("second-writer", "1.0.0", VALUE, 8, "write");
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    r.acquire("first", "writer/scalar").unwrap();
    r.add_maps(vec![resolved(&second, 3)]).unwrap();
    assert_eq!(r.acquire("other", "second-writer/scalar"), Err(Error::Busy), "acquire refuses the overlapping lease");
}

#[test]
fn overlapping_read_aliases_pass_add_maps_as_they_pass_new() {
    let a = map("alias-a", "1.0.0", VALUE, 8, "read");
    let b = map("alias-b", "1.0.0", VALUE, 8, "read");
    assert!(Runtime::new(Fixture { bytes: bytes(), execution: true }, vec![resolved(&a, 1), resolved(&b, 2)]).is_ok());
    let mut r = runtime(vec![resolved(&a, 1)]);
    assert_eq!(r.add_maps(vec![resolved(&b, 2)]), Ok(added(&["alias-b"], &[])));
}

#[test]
fn a_writer_over_a_guard_conflicts_in_both_directions() {
    let guard = map("guard", "1.0.0", OVER_WRITER, 12, "read");
    // An existing writer against the guard of a new symbol.
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    assert_eq!(r.add_maps(vec![resolved(&guard, 2)]), Err(Error::Conflict));
    assert_eq!(r.read("guard/scalar").unwrap_err(), Error::Unavailable("symbol not in this session".into()));
    // A new writer against an existing guard.
    let mut r = runtime(vec![resolved(&guard, 2)]);
    assert_eq!(r.add_maps(vec![resolved(&writer(), 1)]), Err(Error::Conflict));
    assert!(r.acquire("f", "writer/scalar").is_err());
    // The same pair in one `new` fails with the same rule.
    assert!(Runtime::new(Fixture { bytes: bytes(), execution: true }, vec![resolved(&writer(), 1), resolved(&guard, 2)]).is_err());
}

#[test]
fn another_version_or_digest_of_a_map_conflicts() {
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    assert_eq!(r.add_maps(vec![resolved(&writer(), 9)]), Err(Error::Conflict), "same version, other file digest");
    assert_eq!(r.add_maps(vec![resolved(&map("writer", "1.1.0", VALUE, 8, "write"), 1)]), Err(Error::Conflict), "other version");
    // Symbol keys hold the map id, so a duplicate key comes only from one map id: a batch with
    // two identities of one new map is a Conflict, and nothing of it stays.
    let free = map("free", "1.0.0", "46 4D 5F 4F 54 48 45 52", 8, "read");
    assert_eq!(r.add_maps(vec![resolved(&free, 1), resolved(&free, 2)]), Err(Error::Conflict));
    assert_eq!(r.add_maps(vec![resolved(&free, 2)]), Ok(added(&["free"], &[])), "the failed batch added nothing");
}

#[test]
fn a_mixed_batch_changes_nothing() {
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    let free = map("free", "1.0.0", "46 4D 5F 4F 54 48 45 52", 8, "read");
    let guard = map("guard", "1.0.0", OVER_WRITER, 12, "read");
    assert_eq!(r.add_maps(vec![resolved(&free, 1), resolved(&guard, 2)]), Err(Error::Conflict));
    assert!(r.read("free/scalar").is_err(), "the valid map of the failed batch is not there");
    assert_eq!(r.add_maps(vec![resolved(&free, 1)]), Ok(added(&["free"], &[])));
}

#[test]
fn a_map_from_another_module_session_is_unavailable() {
    let mut r = runtime(vec![resolved(&writer(), 1)]);
    let late = map("late", "1.0.0", "46 4D 5F 4F 54 48 45 52", 8, "read").resolve_pinned(&image(18), [1; 32]).unwrap();
    assert!(matches!(r.add_maps(vec![late]), Err(Error::Unavailable(_))));
    assert!(r.read("late/scalar").is_err());
}
