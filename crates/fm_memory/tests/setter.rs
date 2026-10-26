// The mirrored setter contract as observed in MSFS 2024: value and f32 mirror, native clamp,
// and a vtable change handler after a real change. The fixture records handler calls, can refuse
// them and can rewrite the span like a native handler, so compensation and restore retries are
// proved without a game.
// One byte range per readable list is intended in these fixtures.
#![allow(clippy::single_range_in_vec_init)]
use fm_memory::{Error, Image, Map, Memory, ModuleId, Runtime, Scalar};

const LEN: usize = 128;

/// `handler_writes` is (offset, bytes) that the handler stores in the object on each call.
struct Object { bytes: Vec<u8>, execution: bool, fail_call: bool, calls: Vec<(u64, u64)>, writes: usize, handler_writes: Option<(usize, [u8; 4])> }

impl Object {
    /// vtable pointer at 0, pattern at 8, mirror at 16, value at 20, native bounds at 24 and 28,
    /// vtable at 96 whose slot 2 (offset 112) points to a handler at 64 inside the image.
    fn new(value: [u8; 4], mirror: [u8; 4], min: [u8; 4], max: [u8; 4]) -> Self {
        let mut bytes = vec![0; LEN];
        bytes[0..8].copy_from_slice(&96u64.to_le_bytes());
        bytes[8..16].copy_from_slice(b"FM_VALUE");
        bytes[16..20].copy_from_slice(&mirror);
        bytes[20..24].copy_from_slice(&value);
        bytes[24..28].copy_from_slice(&min);
        bytes[28..32].copy_from_slice(&max);
        bytes[112..120].copy_from_slice(&64u64.to_le_bytes());
        Self { bytes, execution: true, fail_call: false, calls: vec![], writes: 0, handler_writes: None }
    }
    fn float(v: f32) -> Self { Self::new(v.to_le_bytes(), v.to_le_bytes(), 0f32.to_le_bytes(), 5f32.to_le_bytes()) }
    fn clone_bytes(&self) -> Self { Self { bytes: self.bytes.clone(), execution: true, fail_call: false, calls: vec![], writes: 0, handler_writes: None } }
    fn f32_at(&self, at: usize) -> f32 { f32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap()) }
}

impl Memory for Object {
    fn identity(&self) -> Result<ModuleId, Error> { Ok(identity()) }
    fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> {
        self.bytes.get(address as usize..address as usize + size).map(|b| b.to_vec()).ok_or(Error::Unavailable("outside image".into()))
    }
    fn can_write(&self) -> bool { self.execution }
    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        if !self.execution { return Err(Error::Unavailable("no execution point".into())); }
        self.writes += 1;
        self.bytes[address as usize..address as usize + data.len()].copy_from_slice(data);
        Ok(())
    }
    fn call(&mut self, function: u64, object: u64) -> Result<(), Error> {
        if !self.execution || self.fail_call { return Err(Error::Unavailable("handler refused".into())); }
        self.calls.push((function, object));
        if let Some((at, bytes)) = self.handler_writes { self.bytes[at..at + 4].copy_from_slice(&bytes); }
        Ok(())
    }
}

fn identity() -> ModuleId { ModuleId { game: "msfs2024".into(), module: "test.exe".into(), sha256: "b".repeat(64), session: 3 } }

fn map(kind: &str, setter: serde_json::Value) -> Map {
    serde_json::from_value(serde_json::json!({
        "schema": 1, "id": "map-setter", "version": "1.0.0", "game": "msfs2024", "module": "test.exe", "sha256": "b".repeat(64),
        "symbols": [{ "name": "tweak", "pattern": "46 4D 5F 56 41 4C 55 45", "offset": 12, "kind": kind, "access": "write",
            "min": 0.0, "max": 10.0, "type_guard": { "offset": -8, "rva": 96 }, "setter": setter }]
    })).unwrap()
}

fn contract() -> serde_json::Value { serde_json::json!({ "contract": "mirrored_notify_v1", "mirror": -4, "clamp_min": 4, "clamp_max": 8, "notify_slot": 2 }) }

fn image(o: &Object) -> Image { Image { base: 0, bytes: o.bytes.clone(), id: identity(), readable: vec![0..LEN] } }

fn runtime(kind: &str, o: Object) -> Runtime<Object> {
    let m = map(kind, contract());
    let resolved = m.resolve(&image(&o)).unwrap();
    Runtime::new(o, vec![resolved]).unwrap()
}

#[test]
fn write_updates_mirror_and_calls_the_handler_only_on_change() {
    let mut r = runtime("f32", Object::float(1.0));
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    r.set("f", h, Scalar::F32(3.0)).unwrap();
    let o = r.memory_mut();
    assert_eq!((o.f32_at(20), o.f32_at(16)), (3.0, 3.0));
    assert_eq!(o.calls, vec![(64, 0)]);
    r.set("f", h, Scalar::F32(3.0)).unwrap();
    assert_eq!(r.memory_mut().calls.len(), 1, "an unchanged value runs no handler, like the native setter");
    r.release("f", h).unwrap();
    let o = r.memory_mut();
    assert_eq!((o.f32_at(20), o.f32_at(16)), (1.0, 1.0));
    assert_eq!(o.calls.len(), 2, "restore uses the same setter semantics");
}

#[test]
fn values_outside_the_native_clamp_are_refused_before_mutation() {
    let mut r = runtime("f32", Object::float(1.0));
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    assert!(matches!(r.set("f", h, Scalar::F32(7.0)), Err(Error::Invalid(_))));
    let o = r.memory_mut();
    assert_eq!((o.writes, o.calls.len(), o.f32_at(20)), (0, 0, 1.0));
}

#[test]
fn a_refused_handler_restores_the_previous_span_at_once() {
    let mut r = runtime("f32", Object::float(1.0));
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    r.memory_mut().fail_call = true;
    assert!(matches!(r.set("f", h, Scalar::F32(2.0)), Err(Error::Unavailable(_))));
    assert_eq!((r.memory_mut().f32_at(20), r.memory_mut().f32_at(16)), (1.0, 1.0));
    assert_eq!(r.set("f", h, Scalar::F32(2.0)), Err(Error::Refused), "a failed lease accepts no more writes");
    r.release("f", h).unwrap();
    assert!(r.memory_mut().calls.is_empty(), "the net value never changed, so no handler is due");
}

#[test]
fn a_failed_restore_handler_keeps_the_lease_and_retries_only_the_call() {
    let mut r = runtime("f32", Object::float(1.0));
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    r.set("f", h, Scalar::F32(4.0)).unwrap();
    r.memory_mut().fail_call = true;
    assert!(r.release("f", h).is_err());
    assert_eq!(r.memory_mut().f32_at(20), 1.0);
    assert_eq!(r.acquire("g", "map-setter/tweak"), Err(Error::Busy), "ownership stays until the handler ran");
    let writes = r.memory_mut().writes;
    r.memory_mut().fail_call = false;
    r.release("f", h).unwrap();
    assert_eq!(r.memory_mut().writes, writes, "the retry does not write again");
    assert_eq!(r.memory_mut().calls.len(), 2);
    r.acquire("g", "map-setter/tweak").unwrap();
}

#[test]
fn a_handler_that_rewrites_the_mirror_leaves_a_restorable_lease() {
    let mut r = runtime("f32", Object::float(1.0));
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    r.memory_mut().handler_writes = Some((16, 3.5f32.to_le_bytes()));
    r.set("f", h, Scalar::F32(3.0)).unwrap();
    assert_eq!(r.memory_mut().f32_at(16), 3.5, "the handler result is the written state");
    r.memory_mut().handler_writes = None;
    r.set("f", h, Scalar::F32(2.0)).unwrap();
    r.release("f", h).unwrap();
    assert_eq!((r.memory_mut().f32_at(20), r.memory_mut().f32_at(16)), (1.0, 1.0));
}

#[test]
fn a_handler_that_changes_the_value_refuses_more_writes_and_still_restores() {
    let mut r = runtime("f32", Object::float(1.0));
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    r.memory_mut().handler_writes = Some((20, 4.5f32.to_le_bytes()));
    assert_eq!(r.set("f", h, Scalar::F32(3.0)), Err(Error::Conflict));
    assert_eq!(r.set("f", h, Scalar::F32(2.0)), Err(Error::Refused));
    r.memory_mut().handler_writes = None;
    r.release("f", h).unwrap();
    assert_eq!((r.memory_mut().f32_at(20), r.memory_mut().f32_at(16)), (1.0, 1.0));
}

#[test]
fn integer_values_keep_a_converted_f32_mirror_and_the_native_clamp() {
    let o = Object::new(4i32.to_le_bytes(), 4f32.to_le_bytes(), 0i32.to_le_bytes(), 10i32.to_le_bytes());
    let mut r = runtime("i32", o);
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    r.memory_mut().handler_writes = Some((16, 7f32.to_le_bytes()));
    r.set("f", h, Scalar::I32(7)).unwrap();
    let o = r.memory_mut();
    assert_eq!((i32::from_le_bytes(o.bytes[20..24].try_into().unwrap()), o.f32_at(16)), (7, 7.0));
    assert_eq!(r.read("map-setter/tweak"), Ok(Scalar::I32(7)));
    assert!(matches!(r.set("f", h, Scalar::I32(11)), Err(Error::Invalid(_))), "the int clamp is read as i32");
    r.memory_mut().handler_writes = Some((16, 4f32.to_le_bytes()));
    r.release("f", h).unwrap();
    let o = r.memory_mut();
    assert_eq!((i32::from_le_bytes(o.bytes[20..24].try_into().unwrap()), o.f32_at(16), o.calls.len()), (4, 4.0, 2));
}

fn bool_contract() -> serde_json::Value { serde_json::json!({ "contract": "mirrored_notify_v1", "mirror": -4, "notify_slot": 2 }) }

#[test]
fn bool_values_write_one_byte_and_a_one_or_zero_mirror() {
    let mut o = Object::new([0, 0xAA, 0xBB, 0x01], 0f32.to_le_bytes(), [0; 4], [0; 4]);
    o.bytes[24..32].fill(0xEE);
    let m = map("bool", bool_contract());
    let mut r = Runtime::new(o.clone_bytes(), vec![m.resolve(&image(&o)).unwrap()]).unwrap();
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    r.set("f", h, Scalar::Bool(true)).unwrap();
    let o = r.memory_mut();
    assert_eq!((&o.bytes[20..24], o.f32_at(16), o.calls.len()), (&[1, 0xAA, 0xBB, 0x01][..], 1.0, 1), "bytes beside the value stay");
    r.release("f", h).unwrap();
    let o = r.memory_mut();
    assert_eq!((&o.bytes[20..24], o.f32_at(16), o.calls.len()), (&[0, 0xAA, 0xBB, 0x01][..], 0.0, 2));
}

#[test]
fn clamp_fields_follow_the_value_kind() {
    assert!(matches!(map("bool", contract()).check(), Err(Error::Invalid(_))), "a bool has no native clamp");
    assert!(matches!(map("f32", bool_contract()).check(), Err(Error::Invalid(_))), "a number needs its native clamp");
    assert!(matches!(map("i32", bool_contract()).check(), Err(Error::Invalid(_))));
    assert!(map("bool", bool_contract()).check().is_ok());
}

#[test]
fn stop_restores_through_the_execution_point_and_reports_its_absence() {
    let mut r = runtime("f32", Object::float(1.0));
    let h = r.acquire("f", "map-setter/tweak").unwrap();
    r.set("f", h, Scalar::F32(2.0)).unwrap();
    r.memory_mut().execution = false;
    assert_eq!(r.stop("f").len(), 1, "no execution point: cleanup fails visibly");
    assert_eq!(r.memory_mut().f32_at(20), 2.0);
    r.memory_mut().execution = true;
    assert!(r.stop("f").is_empty());
    assert_eq!((r.memory_mut().f32_at(20), r.memory_mut().calls.len()), (1.0, 2));
}

#[test]
fn setter_maps_without_type_proof_or_with_foreign_handlers_never_resolve() {
    let o = Object::float(1.0);
    let mut m = map("f32", contract());
    m.symbols[0].type_guard = None;
    assert!(matches!(m.check(), Err(Error::Invalid(_))));
    let m = map("f32", serde_json::json!({ "contract": "mirrored_notify_v1", "mirror": 0, "clamp_min": 4, "clamp_max": 8, "notify_slot": 2 }));
    assert!(matches!(m.check(), Err(Error::Invalid(_))), "a mirror must not alias the value");
    let m = map("bool", contract());
    assert!(matches!(m.check(), Err(Error::Invalid(_))));
    let mut foreign = Object::float(1.0);
    foreign.bytes[112..120].copy_from_slice(&0x1_0000u64.to_le_bytes());
    assert!(matches!(map("f32", contract()).resolve(&image(&foreign)), Err(Error::Unavailable(_))), "handler outside the module");
    assert!(serde_json::from_value::<Map>(serde_json::json!({
        "schema": 1, "id": "map-setter", "version": "1.0.0", "game": "msfs2024", "module": "test.exe", "sha256": "b".repeat(64),
        "symbols": [{ "name": "tweak", "pattern": "46 4D 5F 56 41 4C 55 45", "offset": 12, "kind": "f32", "access": "write", "min": 0.0, "max": 10.0,
            "type_guard": { "offset": -8, "rva": 96 }, "setter": { "contract": "mirrored_notify_v1", "mirror": -4, "clamp_min": 4, "clamp_max": 8, "notify_slot": 2, "function": 64 } }]
    })).is_err(), "a map cannot supply a handler address");
    drop(o);
}
