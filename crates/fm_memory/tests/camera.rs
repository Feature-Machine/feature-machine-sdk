// camera-projection-v1 through the public module: a RIP-anchored static, pointer steps at each
// read, and the projection shape check. The fixture models MSFS 1.9.20.0 (plan 7.5): code that
// loads the static, a heap object, and the view pool slot with a column-layout projection.
// One byte range per readable list is intended in these fixtures.
#![allow(clippy::single_range_in_vec_init)]
use fm_memory::{Error, Image, Map, Memory, ModuleId, Runtime, Scalar};
use std::collections::BTreeMap;

const STATIC: u64 = 0x40;
const OBJECT: u64 = 0x2_0000;
const POOL: u64 = 0x3_0000;

/// Sparse memory: the image at 0, heap blocks elsewhere. Unmapped reads fail like a bad pointer.
struct Fixture { blocks: BTreeMap<u64, Vec<u8>>, heap: bool }

impl Memory for Fixture {
    fn identity(&self) -> Result<ModuleId, Error> { Ok(identity()) }
    fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> {
        let (base, b) = self.blocks.range(..=address).next_back().ok_or(Error::Unavailable("unmapped".into()))?;
        let at = (address - base) as usize;
        b.get(at..at + size).map(<[u8]>::to_vec).ok_or(Error::Unavailable("unmapped".into()))
    }
    fn can_write(&self) -> bool { false }
    fn write(&mut self, _: u64, _: &[u8]) -> Result<(), Error> { Err(Error::Refused) }
    fn read_heap(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> { if self.heap { self.read(address, size) } else { Err(Error::Unavailable("no heap read".into())) } }
}

fn identity() -> ModuleId { ModuleId { game: "msfs2024".into(), module: "test.exe".into(), sha256: "a".repeat(64), session: 3 } }

/// Code at 0: `mov rcx, [rip+rel32]` (48 8B 0D) then four marker bytes; rel32 ends the
/// instruction at 7, so the static is at 7 + rel32.
fn image_bytes() -> Vec<u8> {
    let mut b = vec![0u8; 0x100];
    b[0..3].copy_from_slice(&[0x48, 0x8B, 0x0D]);
    b[3..7].copy_from_slice(&((STATIC as i32) - 7).to_le_bytes());
    b[7..11].copy_from_slice(&[0xC3, 0xCC, 0xF0, 0x0D]);
    b[STATIC as usize..STATIC as usize + 8].copy_from_slice(&OBJECT.to_le_bytes());
    b
}

/// MSFS layout: column order, near in m[11], 1 in m[14], TAA jitter in m[2] and m[6].
fn matrix(vfov_deg: f32, aspect: f32, near: f32) -> Vec<u8> {
    let y = 1.0 / (vfov_deg.to_radians() * 0.5).tan();
    let m = [y / aspect, 0.0, 0.0001, 0.0, 0.0, y, -0.0003, 0.0, 0.0, 0.0, -1.6e-7, near, 0.0, 0.0, 1.0, 0.0];
    m.iter().flat_map(|v: &f32| v.to_le_bytes()).collect()
}

fn fixture(slot: Vec<u8>) -> Fixture {
    let mut object = vec![0u8; 0x20];
    object[0x10..0x18].copy_from_slice(&POOL.to_le_bytes());
    let mut pool = vec![0u8; 0x100];
    pool[0x40..0x80].copy_from_slice(&slot);
    Fixture { blocks: BTreeMap::from([(0, image_bytes()), (OBJECT, object), (POOL, pool)]), heap: true }
}

fn map(symbol: serde_json::Value) -> Map {
    serde_json::from_value(serde_json::json!({
        "schema": 1, "id": "msfs2024-camera", "version": "1.0.0", "game": "msfs2024", "module": "test.exe",
        "sha256": "a".repeat(64), "symbols": [symbol]
    })).unwrap()
}

fn camera() -> serde_json::Value {
    serde_json::json!({ "name": "view-projection", "pattern": "48 8B 0D ?? ?? ?? ?? C3 CC F0 0D", "offset": 0, "rip": 3,
        "steps": [0x10, 0x40], "kind": "projection", "access": "read", "min": 5.0, "max": 150.0 })
}

fn runtime(m: &Map, f: Fixture) -> Result<Runtime<Fixture>, Error> {
    let image = Image { base: 0, bytes: image_bytes(), id: identity(), readable: vec![0..0x100] };
    Runtime::new(f, vec![m.resolve(&image)?])
}

fn near_vfov(v: Scalar) -> (f32, f32) {
    let Scalar::Camera { near, tan_half_v, .. } = v else { panic!("not a camera: {v:?}") };
    (near, (2.0 * tan_half_v.atan()).to_degrees())
}

#[test]
fn reads_the_live_projection_through_the_steps() {
    let m = map(camera());
    let mut r = runtime(&m, fixture(matrix(49.22, 2.4277, 0.059))).unwrap();
    let (near, vfov) = near_vfov(r.read("msfs2024-camera/view-projection").unwrap());
    assert_eq!(near, 0.059);
    assert!((vfov - 49.22).abs() < 1e-3, "{vfov}");
    // A zoom changes the slot; the next read follows (steps are taken again at each read).
    r.memory_mut().blocks.get_mut(&POOL).unwrap()[0x40..0x80].copy_from_slice(&matrix(19.09, 2.4277, 0.059));
    assert!((near_vfov(r.read("msfs2024-camera/view-projection").unwrap()).1 - 19.09).abs() < 1e-3);
}

// read_raw gives the static, each step target and the slot bytes; read is read_raw then
// decode, so a tool that records a failed read sees the bytes that the host refused.
#[test]
fn a_raw_read_names_the_chain_and_the_slot_bytes() {
    let m = map(camera());
    let key = "msfs2024-camera/view-projection";
    let r = runtime(&m, fixture(matrix(49.22, 2.4277, 0.059))).unwrap();
    let raw = r.read_raw(key).unwrap();
    assert_eq!(raw.chain, vec![STATIC, OBJECT + 0x10, POOL + 0x40]);
    assert_eq!(raw.bytes, matrix(49.22, 2.4277, 0.059));
    assert_eq!(r.decode(key, &raw).unwrap(), r.read(key).unwrap());
}

// Live 2026-10-21 (`drops/` traces): the exact slot also holds other views within a frame, a
// perspective with near 0.1 m and far 1 m, and an orthographic view. Those reads are OtherView
// (no sample, slot intact); other bytes, and a scene camera with a bad value, stay Unavailable.
#[test]
fn other_views_in_the_slot_are_named_apart_from_bad_bytes() {
    let m = map(camera());
    let key = "msfs2024-camera/view-projection";
    let bytes = |m: [f32; 16]| m.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
    let finite_far = [41.19049, 0.0, -0.00003, 0.0, 0.0, 99.99667, 0.00046, 0.0, 0.0, 0.0, -0.11111, 0.11111, 0.0, 0.0, 1.0, 0.0];
    let orthographic = [0.00048, 0.0, 0.0, 0.0, 0.0, 0.00048, 0.0, 0.0, 0.0, 0.0, -0.00024, 1.0, 0.0, 0.0, 0.0, 1.0];
    for view in [finite_far, orthographic] {
        assert_eq!(runtime(&m, fixture(bytes(view))).unwrap().read(key), Err(Error::OtherView), "{view:?}");
    }
    let mut jitter_too_large = finite_far;
    (jitter_too_large[10], jitter_too_large[11], jitter_too_large[2]) = (-1.6e-7, 0.059, 0.5);
    let garbage = [3.0e9; 16];
    // Review round 1: shapes close to the captured views but malformed are not OtherView.
    let mut negative_near = finite_far;
    negative_near[11] = -0.11111;
    let mut flat_orthographic = orthographic;
    flat_orthographic[10] = 0.0;
    for bad in [jitter_too_large, garbage, negative_near, flat_orthographic] {
        assert!(matches!(runtime(&m, fixture(bytes(bad))).unwrap().read(key), Err(Error::Unavailable(_))), "{bad:?}");
    }
}

#[test]
fn other_views_and_bad_pointers_are_not_a_camera() {
    let m = map(camera());
    let key = "msfs2024-camera/view-projection";
    // A cube view in the slot (row layout: 1 in m[11], near in m[14]) fails the shape check.
    let mut cube = matrix(90.0, 1.0, 0.049);
    cube[44..48].copy_from_slice(&1.0f32.to_le_bytes());
    cube[56..60].copy_from_slice(&0.049f32.to_le_bytes());
    assert!(matches!(runtime(&m, fixture(cube)).unwrap().read(key), Err(Error::Unavailable(_))));
    // Outside the map's FOV range.
    assert!(matches!(runtime(&m, fixture(matrix(170.0, 1.5, 0.059))).unwrap().read(key), Err(Error::Invalid(_))));
    // No flight yet: the object holds a null pointer. Resolution still works; the read fails.
    let mut f = fixture(matrix(49.22, 2.4277, 0.059));
    f.blocks.get_mut(&OBJECT).unwrap()[0x10..0x18].copy_from_slice(&0u64.to_le_bytes());
    assert!(matches!(runtime(&m, f).unwrap().read(key), Err(Error::Unavailable(_))));
    // An adapter without heap reads (the default) never follows a step off the image.
    let f = Fixture { heap: false, ..fixture(matrix(49.22, 2.4277, 0.059)) };
    assert!(matches!(runtime(&m, f).unwrap().read(key), Err(Error::Unavailable(_))));
}

#[test]
fn the_contract_refuses_writes_and_unbounded_steps() {
    let refuse = |change: serde_json::Value| {
        let mut s = camera();
        for (k, v) in change.as_object().unwrap() { s[k] = v.clone(); }
        map(s).check().unwrap_err()
    };
    refuse(serde_json::json!({ "access": "write" }));
    refuse(serde_json::json!({ "rip": 9 }));
    refuse(serde_json::json!({ "rip": null }));
    refuse(serde_json::json!({ "steps": [1, 2, 3, 4, 5, 6, 7, 8, 9] }));
    refuse(serde_json::json!({ "steps": [-8] }));
    assert!(map(camera()).check().is_ok());
    let m = map(camera());
    let mut r = runtime(&m, fixture(matrix(49.22, 2.4277, 0.059))).unwrap();
    assert_eq!(r.acquire("feature", "msfs2024-camera/view-projection"), Err(Error::Refused));
}
