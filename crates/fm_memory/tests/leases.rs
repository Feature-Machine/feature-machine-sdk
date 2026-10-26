// Exercise resolution and restoration through the public module, including hostile maps and
// external state changes. The memory fixture enforces one execution thread like a host adapter.
// One byte range per readable list is intended in these fixtures.
#![allow(clippy::single_range_in_vec_init)]
use fm_memory::{Access, Error, Image, Map, Memory, ModuleId, Runtime, Scalar};

struct Fixture { bytes: Vec<u8>, id: ModuleId, execution: bool, fail_write: bool, partial: Option<usize>, unknown: bool }

impl Fixture {
    fn new() -> Self {
        let mut bytes = vec![0; 64];
        bytes[8..16].copy_from_slice(b"FM_VALUE");
        bytes[16..20].copy_from_slice(&2.5f32.to_le_bytes());
        Self { bytes, id: identity(), execution: true, fail_write: false, partial: None, unknown: false }
    }
}

impl Memory for Fixture {
    fn identity(&self) -> Result<ModuleId, Error> { Ok(self.id.clone()) }
    fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> {
        self.bytes.get(address as usize..address as usize + size).map(|b| b.to_vec()).ok_or(Error::Unavailable("outside image".into()))
    }
    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        if !self.execution { return Err(Error::Unavailable("no execution point".into())); }
        if self.fail_write { return Err(Error::Unavailable("fixture write failed before mutation".into())); }
        if let Some(n) = self.partial.take() {
            self.bytes[address as usize..address as usize + n].copy_from_slice(&data[..n]);
            if self.unknown { return Err(Error::Unavailable("unqualified partial result".into())); }
            return Err(Error::PartialWrite { detail: "fixture partial result".into(), bytes: self.bytes[address as usize..address as usize + data.len()].to_vec() });
        }
        self.bytes[address as usize..address as usize + data.len()].copy_from_slice(data);
        Ok(())
    }
    fn can_write(&self) -> bool { self.execution }
}

fn identity() -> ModuleId {
    ModuleId { game: "msfs2024".into(), module: "test.exe".into(), sha256: "a".repeat(64), session: 17 }
}

fn map() -> Map {
    serde_json::from_value(serde_json::json!({
        "schema": 1, "id": "map-test", "version": "1.0.0", "game": "msfs2024",
        "module": "test.exe", "sha256": "a".repeat(64),
        "symbols": [{ "name": "scalar", "pattern": "46 4D 5F 56 41 4C 55 45", "offset": 8,
            "kind": "f32", "access": "write", "min": 0.0, "max": 10.0 }]
    })).unwrap()
}

fn runtime(m: &Map, f: Fixture) -> Runtime<Fixture> {
    let image = Image { base: 0, bytes: f.bytes.clone(), id: f.id.clone(), readable: vec![0..64] };
    Runtime::new(f, vec![m.resolve(&image).unwrap()]).unwrap()
}

#[test]
fn only_one_writer_and_release_restores_the_first_value() {
    let m = map();
    let mut r = runtime(&m, Fixture::new());
    let h = r.acquire("first", "map-test/scalar").unwrap();
    assert_eq!(r.acquire("second", "map-test/scalar"), Err(Error::Busy));
    assert_eq!(r.set("second", h, Scalar::F32(7.0)), Err(Error::Refused));
    r.set("first", h, Scalar::F32(4.0)).unwrap();
    r.set("first", h, Scalar::F32(6.0)).unwrap();
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::F32(6.0));
    r.release("first", h).unwrap();
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::F32(2.5));
    assert!(r.set("first", h, Scalar::F32(3.0)).is_err());
}

#[test]
fn stop_revokes_handles_and_cannot_resume_writes() {
    let mut r = runtime(&map(), Fixture::new());
    let h = r.acquire("first", "map-test/scalar").unwrap();
    r.set("first", h, Scalar::F32(4.0)).unwrap();
    assert!(r.stop("first").is_empty());
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::F32(2.5));
    assert_eq!(r.acquire("first", "map-test/scalar"), Err(Error::Refused));
    assert!(r.stop("first").is_empty());
}

#[test]
fn wrong_build_ambiguous_and_unbounded_maps_never_resolve() {
    let f = Fixture::new();
    let mut image = Image { base: 0, bytes: f.bytes, id: f.id, readable: vec![0..64] };
    let mut m = map();
    image.id.sha256 = "b".repeat(64);
    assert!(m.resolve(&image).is_err());
    image.id.sha256 = "a".repeat(64);
    image.bytes[32..40].copy_from_slice(b"FM_VALUE");
    assert!(m.resolve(&image).is_err());
    image.bytes[32..40].fill(0);
    m.symbols[0].offset = i64::MAX;
    assert!(m.resolve(&image).is_err());
    m.symbols[0].offset = 0;
    assert!(m.resolve(&image).is_err(), "mutable storage cannot overlap its immutable guard");
}

#[test]
fn type_range_and_execution_point_are_checked_before_mutation() {
    let mut r = runtime(&map(), Fixture::new());
    let h = r.acquire("first", "map-test/scalar").unwrap();
    assert!(r.set("first", h, Scalar::F32(f32::NAN)).is_err());
    assert!(r.set("first", h, Scalar::F32(12.0)).is_err());
    assert!(r.set("first", h, Scalar::I32(4)).is_err());
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::F32(2.5));
    let mut f = Fixture::new();
    f.execution = false;
    let mut r = runtime(&map(), f);
    assert!(r.acquire("first", "map-test/scalar").is_err());
}

#[test]
fn overlapping_aliases_share_ownership() {
    let mut m = map();
    let mut alias = m.symbols[0].clone();
    alias.name = "alias".into();
    m.symbols.push(alias);
    let mut r = runtime(&m, Fixture::new());
    r.acquire("first", "map-test/scalar").unwrap();
    assert_eq!(r.acquire("second", "map-test/alias"), Err(Error::Busy));
}

#[test]
fn reads_need_no_write_execution_and_read_only_maps_refuse_acquisition() {
    let mut m = map();
    m.symbols[0].access = Access::Read;
    let mut r = runtime(&m, Fixture::new());
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::F32(2.5));
    assert_eq!(r.acquire("first", "map-test/scalar"), Err(Error::Refused));
}

#[test]
fn identity_change_never_restores_into_a_new_session() {
    let mut r = runtime(&map(), Fixture::new());
    let h = r.acquire("first", "map-test/scalar").unwrap();
    r.set("first", h, Scalar::F32(4.0)).unwrap();
    r.memory_mut().id.session += 1;
    assert!(r.read("map-test/scalar").is_err());
    assert_eq!(r.stop("first").len(), 1);
    assert_eq!(f32::from_le_bytes(r.memory_mut().bytes[16..20].try_into().unwrap()), 4.0);
}

#[test]
fn external_change_reports_cleanup_failure_and_keeps_the_range_reserved() {
    let mut r = runtime(&map(), Fixture::new());
    let h = r.acquire("first", "map-test/scalar").unwrap();
    r.set("first", h, Scalar::F32(4.0)).unwrap();
    r.memory_mut().bytes[16..20].copy_from_slice(&8.0f32.to_le_bytes());
    assert_eq!(r.release("first", h), Err(Error::Conflict));
    assert_eq!(r.acquire("second", "map-test/scalar"), Err(Error::Busy));
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::F32(8.0));
    assert_eq!(r.stop("first").len(), 1);
}

// A failed second write must keep the last confirmed bytes available for restoration.
#[test]
fn unchanged_failed_write_can_restore_the_original() {
    let mut r = runtime(&map(), Fixture::new());
    let h = r.acquire("first", "map-test/scalar").unwrap();
    r.set("first", h, Scalar::F32(4.0)).unwrap();
    r.memory_mut().fail_write = true;
    assert!(r.set("first", h, Scalar::F32(6.0)).is_err());
    r.memory_mut().fail_write = false;
    assert!(r.stop("first").is_empty());
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::F32(2.5));
}

#[test]
fn disconnect_closes_admission_for_every_owner() {
    let mut r = runtime(&map(), Fixture::new());
    assert!(r.disconnect().is_empty());
    assert_eq!(r.acquire("new-owner", "map-test/scalar"), Err(Error::Refused));
}

// Only operation-proven intermediate bytes permit compensation. Unknown bytes remain reserved.
#[test]
fn partial_write_compensates_and_release_retries_a_partial_restore() {
    let mut m = map();
    m.symbols[0].kind = fm_memory::Kind::I32;
    m.symbols[0].max = i32::MAX as f64;
    let mut f = Fixture::new();
    f.bytes[16..20].copy_from_slice(&0x11223344i32.to_le_bytes());
    let mut r = runtime(&m, f);
    let h = r.acquire("first", "map-test/scalar").unwrap();
    r.memory_mut().partial = Some(2);
    assert!(r.set("first", h, Scalar::I32(0x55667788)).is_err());
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::I32(0x11223344));
    assert_eq!(r.set("first", h, Scalar::I32(1)), Err(Error::Refused));
    r.release("first", h).unwrap();
    let h = r.acquire("second", "map-test/scalar").unwrap();
    r.set("second", h, Scalar::I32(0x55667788)).unwrap();
    r.memory_mut().partial = Some(2);
    assert!(r.release("second", h).is_err());
    r.release("second", h).unwrap();
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::I32(0x11223344));
}

#[test]
fn unknown_partial_write_keeps_foreign_bytes_and_ownership() {
    let mut m = map();
    m.symbols[0].kind = fm_memory::Kind::I32;
    m.symbols[0].max = i32::MAX as f64;
    let mut f = Fixture::new();
    f.bytes[16..20].copy_from_slice(&0x11223344i32.to_le_bytes());
    let mut r = runtime(&m, f);
    let h = r.acquire("first", "map-test/scalar").unwrap();
    r.memory_mut().unknown = true;
    r.memory_mut().partial = Some(2);
    assert!(r.set("first", h, Scalar::I32(0x55667788)).is_err());
    assert_eq!(r.release("first", h), Err(Error::Conflict));
    assert_eq!(r.acquire("second", "map-test/scalar"), Err(Error::Busy));
    assert_eq!(r.read("map-test/scalar").unwrap(), Scalar::I32(0x11227788));
}

// Masked seed searches must reject unreadable holes and count overlapping full matches.
#[test]
fn masked_patterns_and_unreadable_regions_preserve_unique_resolution() {
    let f = Fixture::new();
    let mut image = Image { base: 0, bytes: f.bytes, id: f.id, readable: vec![0..64] };
    let mut m = map();
    m.symbols[0].pattern = "46 4D ?? ?? 41 4C 55 45".into();
    assert!(m.resolve(&image).is_ok());
    image.readable = vec![0..12, 13..64];
    assert!(m.resolve(&image).is_err());
    image.readable = vec![0..64];
    image.bytes[32..40].copy_from_slice(b"FM__ALUE");
    assert!(m.resolve(&image).is_err());
}

#[test]
fn object_type_is_checked_at_resolution_and_each_access() {
    let mut f = Fixture::new();
    f.bytes[..8].copy_from_slice(&48u64.to_le_bytes());
    let mut m = map();
    m.symbols[0].type_guard = Some(fm_memory::PointerGuard { offset: -8, rva: 48 });
    let image = Image { base: 0, bytes: f.bytes.clone(), id: f.id.clone(), readable: vec![0..64] };
    let mut r = Runtime::new(f, vec![m.resolve(&image).unwrap()]).unwrap();
    let h = r.acquire("first", "map-test/scalar").unwrap();
    r.memory_mut().bytes[..8].copy_from_slice(&40u64.to_le_bytes());
    assert!(r.read("map-test/scalar").is_err());
    assert!(r.set("first", h, Scalar::F32(4.0)).is_err());
    assert!(r.release("first", h).is_err());
    let mut wrong = image;
    wrong.bytes[..8].fill(0);
    assert!(m.resolve(&wrong).is_err());
}
