// camera-locator-v1 through the public module. The fixture models what R2 found live in MSFS
// 1.9.20.0 (plan 2026-10-20-camera-map-updates section 4): many projection-shaped blocks, a few
// plausible copies of the camera (some with frame jitter), and stale or other views beside them.
use fm_memory::{Error, Live, Locator, Memory, Miss, ModuleId, Scalar, FRESH};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const ASPECT: f32 = 2.4277;

/// Heap blocks only. Reads of a `live` address get a new TAA jitter each time, as a frame does.
struct Heap { blocks: BTreeMap<u64, RefCell<Vec<u8>>>, live: RefCell<Vec<u64>>, frame: Cell<u32>, scan: bool }

impl Memory for Heap {
    fn identity(&self) -> Result<ModuleId, Error> { Ok(ModuleId { game: "msfs2024".into(), module: "test.exe".into(), sha256: "a".repeat(64), session: 1 }) }
    fn read(&self, _: u64, _: usize) -> Result<Vec<u8>, Error> { Err(Error::Unavailable("no image".into())) }
    fn can_write(&self) -> bool { false }
    fn write(&mut self, _: u64, _: &[u8]) -> Result<(), Error> { Err(Error::Refused) }
    fn read_heap(&self, address: u64, size: usize) -> Result<Vec<u8>, Error> {
        let (base, b) = self.blocks.range(..=address).next_back().ok_or(Error::Unavailable("unmapped".into()))?;
        let at = (address - base) as usize;
        let mut out = b.borrow().get(at..at + size).ok_or(Error::Unavailable("unmapped".into()))?.to_vec();
        if self.live.borrow().contains(&address) {
            self.frame.set(self.frame.get() + 1);
            out[8..12].copy_from_slice(&(0.0001 + self.frame.get() as f32 * 1e-6).to_le_bytes());
        }
        Ok(out)
    }
    fn scan(&self, visit: &mut dyn FnMut(u64, &[u8]) -> bool) -> Result<(), Error> {
        if !self.scan { return Err(Error::Unavailable("no memory scan".into())); }
        for (base, b) in &self.blocks { if !visit(*base, &b.borrow()) { break; } }
        Ok(())
    }
}

/// MSFS layout: column order, near in m[11], 1 in m[14], TAA jitter in m[2] and m[6].
fn matrix(vfov_deg: f32, aspect: f32, near: f32) -> Vec<u8> {
    let y = 1.0 / (vfov_deg.to_radians() * 0.5).tan();
    let m = [y / aspect, 0.0, 0.0001, 0.0, 0.0, y, -0.0003, 0.0, 0.0, 0.0, -1.6e-7, near, 0.0, 0.0, 1.0, 0.0];
    m.iter().flat_map(|v: &f32| v.to_le_bytes()).collect()
}

const POOL: u64 = 0x10_0000;
const COPY: u64 = 0x20_0000;
const OTHER: u64 = 0x30_0000;

/// The pool has the live camera at +0x260 and a stale copy of an old zoom at +0x2a0; a second
/// block has another live copy; a third has an unjittered copy, a shadow view (aspect 1) and a
/// near-zero shape.
fn heap() -> Heap {
    let mut pool = vec![0u8; 0x1000];
    pool[0x260..0x2a0].copy_from_slice(&matrix(49.22, ASPECT, 0.059));
    pool[0x2a0..0x2e0].copy_from_slice(&matrix(19.09, ASPECT, 0.059));
    let mut copy = vec![0u8; 0x1000];
    copy[0x40..0x80].copy_from_slice(&matrix(49.22, ASPECT, 0.059));
    let mut other = vec![0u8; 0x1000];
    other[0x100..0x140].copy_from_slice(&matrix(90.0, 1.0, 0.5));
    other[0x200..0x240].copy_from_slice(&matrix(49.22, ASPECT, 0.0000001));
    other[0x300..0x340].copy_from_slice(&matrix(49.22, ASPECT, 0.059));
    Heap { blocks: BTreeMap::from([(POOL, RefCell::new(pool)), (COPY, RefCell::new(copy)), (OTHER, RefCell::new(other))]),
        live: RefCell::new(vec![POOL + 0x260, COPY + 0x40]), frame: Cell::new(0), scan: true }
}

fn locator() -> Locator {
    serde_json::from_value(serde_json::json!({ "schema": 1, "id": "msfs2024-camera-locator", "version": "1.0.0", "game": "msfs2024",
        "module": "test.exe", "near": [0.005, 2.0], "fov": [5.0, 150.0], "aspect_tolerance": 0.01 })).unwrap()
}

fn vfov(v: Scalar) -> f32 { let Scalar::Camera { tan_half_v, .. } = v else { panic!("not a camera: {v:?}") }; (2.0 * tan_half_v.atan()).to_degrees() }
fn go(_: Duration) -> bool { true }

#[test]
fn finds_the_camera_without_build_offsets() {
    let h = heap();
    let l = locator();
    let mut found = l.find(&h, ASPECT, &mut go).unwrap();
    // The two live copies agree; the unjittered copy (not proved live), the stale zoom, the
    // shadow view and the near-zero block are not kept.
    assert_eq!(found.len(), 2);
    assert!((vfov(found.read(&h, &l, ASPECT, Instant::now()).unwrap()) - 49.22).abs() < 1e-3);
}

#[test]
fn reads_take_the_majority_and_refuse_a_tie() {
    let h = heap();
    // A third live copy, so a zoom that reaches only two of three still has a majority.
    h.live.borrow_mut().push(OTHER + 0x300);
    let l = locator();
    let mut found = l.find(&h, ASPECT, &mut go).unwrap();
    assert_eq!(found.len(), 3);
    for (block, at) in [(POOL, 0x260), (COPY, 0x40)] { h.blocks[&block].borrow_mut()[at..at + 64].copy_from_slice(&matrix(30.0, ASPECT, 0.059)); }
    let now = Instant::now();
    assert!((vfov(found.read(&h, &l, ASPECT, now).unwrap()) - 30.0).abs() < 1e-3, "two copies outvote one that missed the zoom");
    h.blocks[&COPY].borrow_mut()[0x40..0x80].copy_from_slice(&[0u8; 64]);
    assert!(found.read(&h, &l, ASPECT, now).is_err(), "one against one is a tie: no camera");
    // A window shape that differs from the projection fails every copy.
    assert!(found.read(&h, &l, 1.5, now).is_err());
}

// Live 2026-10-20: during a zoom MSFS holds each view for this frame and the previous one, so the
// copies split in two equal groups one frame step apart. That is one camera, not a tie; two
// cameras far apart with equal support still are.
#[test]
fn copies_one_frame_apart_are_one_camera() {
    let h = heap();
    h.blocks[&OTHER].borrow_mut()[0x380..0x3c0].copy_from_slice(&matrix(49.22, ASPECT, 0.059));
    h.live.borrow_mut().extend([OTHER + 0x300, OTHER + 0x380]);
    let l = locator();
    let mut found = l.find(&h, ASPECT, &mut go).unwrap();
    assert_eq!(found.len(), 4);
    for (block, at, fov) in [(POOL, 0x260, 44.03), (COPY, 0x40, 44.51), (OTHER, 0x300, 44.03), (OTHER, 0x380, 44.51)] {
        h.blocks[&block].borrow_mut()[at..at + 64].copy_from_slice(&matrix(fov, ASPECT, 0.059));
    }
    let v = vfov(found.read(&h, &l, ASPECT, Instant::now()).unwrap());
    assert!((v - 44.03).abs() < 1e-3 || (v - 44.51).abs() < 1e-3, "one of the two frames: {v}");
    for (block, at) in [(COPY, 0x40), (OTHER, 0x380)] { h.blocks[&block].borrow_mut()[at..at + 64].copy_from_slice(&matrix(30.0, ASPECT, 0.059)); }
    assert!(found.read(&h, &l, ASPECT, Instant::now()).is_err(), "two cameras with equal support are a tie");
}

// Review 2026-10-20: the scan itself can start during a zoom. The split copies are found as one
// camera, and its value is the larger exact group (at most one frame old).
#[test]
fn a_scan_during_a_zoom_finds_one_camera() {
    let h = heap();
    h.live.borrow_mut().extend([OTHER + 0x300, OTHER + 0x380]);
    let set = |fovs: [f32; 4]| for ((block, at), fov) in [(POOL, 0x260), (COPY, 0x40), (OTHER, 0x300), (OTHER, 0x380)].into_iter().zip(fovs) {
        h.blocks[&block].borrow_mut()[at..at + 64].copy_from_slice(&matrix(fov, ASPECT, 0.059));
    };
    let l = locator();
    set([44.03, 44.51, 44.03, 44.51]);
    let mut found = l.find(&h, ASPECT, &mut go).unwrap();
    assert_eq!(found.len(), 4, "both frames are kept");
    set([44.03, 44.51, 44.51, 44.51]);
    assert!((vfov(found.read(&h, &l, ASPECT, Instant::now()).unwrap()) - 44.51).abs() < 1e-3, "the larger exact group");
}

// Review 2026-10-20: the frame step is symmetric. Two frames 5.1% apart are one camera in both
// address orders; before the fix, the larger value first gave two groups and a tie.
#[test]
fn the_frame_step_does_not_depend_on_the_order() {
    let small = 44.0f32;
    let large = 2.0 * (1.051 * (small.to_radians() * 0.5).tan()).atan().to_degrees();
    for (first, second) in [(small, large), (large, small)] {
        let h = heap();
        h.blocks[&OTHER].borrow_mut()[0x380..0x3c0].copy_from_slice(&matrix(49.22, ASPECT, 0.059));
        h.live.borrow_mut().extend([OTHER + 0x300, OTHER + 0x380]);
        let l = locator();
        let mut found = l.find(&h, ASPECT, &mut go).unwrap();
        for ((block, at), fov) in [(POOL, 0x260), (COPY, 0x40), (OTHER, 0x300), (OTHER, 0x380)].into_iter().zip([first, first, second, second]) {
            h.blocks[&block].borrow_mut()[at..at + 64].copy_from_slice(&matrix(fov, ASPECT, 0.059));
        }
        assert!(found.read(&h, &l, ASPECT, Instant::now()).is_ok(), "{first} first, then {second}");
    }
}

// Code review round 1: a copy that stays readable after its view is gone (a freed pool, a closed
// flight) keeps an old value. When its jitter stops, it is out after FRESH, also when it is the
// only copy that is left; the host then clears the camera and scans again.
#[test]
fn a_copy_whose_jitter_stops_is_stale() {
    let h = heap();
    let l = locator();
    let mut found = l.find(&h, ASPECT, &mut go).unwrap();
    let t = Instant::now();
    assert!(found.read(&h, &l, ASPECT, t).is_ok());
    h.blocks[&POOL].borrow_mut()[0x260..0x2a0].copy_from_slice(&[0u8; 64]);
    h.live.borrow_mut().clear();
    assert!(found.read(&h, &l, ASPECT, t + Duration::from_millis(10)).is_ok(), "within FRESH the last copy still counts");
    assert!(found.read(&h, &l, ASPECT, t + FRESH + Duration::from_millis(20)).is_err(), "a copy with stopped jitter is stale");
    // Without anti-aliasing no copy jitters: no copy is proved live, so there is no camera.
    assert!(matches!(l.find(&h, ASPECT, &mut go), Err(Error::Unavailable(_))));
}

#[test]
fn liveness_needs_a_jitter_change_within_fresh() {
    let cam = |j: f32| Scalar::Camera { near: 0.059, tan_half_v: 0.45, aspect: ASPECT, jitter: [j, 0.0] };
    let mut live = Live::default();
    let t = Instant::now();
    assert!(!live.see(&cam(0.1), t), "one read proves nothing");
    assert!(!live.see(&cam(0.1), t + Duration::from_millis(8)), "the same jitter proves nothing");
    assert!(live.see(&cam(0.2), t + Duration::from_millis(16)));
    assert!(live.see(&cam(0.2), t + Duration::from_millis(16) + FRESH));
    assert!(!live.see(&cam(0.2), t + Duration::from_millis(17) + FRESH));
    assert!(!live.see(&Scalar::F32(1.0), t), "only a camera can be live");
    // Review round 2: after a gap without reads (the exact map answered, a failed read), a new
    // jitter proves nothing until the next read close to it shows another change.
    let later = t + Duration::from_secs(60);
    assert!(!live.see(&cam(0.3), later), "a change across a gap is no proof");
    assert!(!live.see(&cam(0.3), later + Duration::from_millis(8)));
    assert!(live.see(&cam(0.4), later + Duration::from_millis(16)));
}

// Review round 3: the proof of a found copy starts at its last real jitter change, not at the end
// of the sampling. A copy that jittered early in the sampling and then stopped is not kept.
#[test]
fn a_copy_that_stops_during_the_sampling_is_not_kept() {
    let h = heap();
    let l = locator();
    let mut gaps = 0;
    let mut wait = |d: Duration| {
        if d > Duration::ZERO {
            gaps += 1;
            if gaps == 2 { h.live.borrow_mut().clear(); }
            std::thread::sleep(Duration::from_millis(60));
        }
        true
    };
    // The last change is at the third sample (about 120 ms); the eighth ends at about 420 ms.
    assert!(matches!(l.find(&h, ASPECT, &mut wait), Err(Error::Unavailable(_))));
}

#[test]
fn a_located_set_resumed_after_a_gap_proves_itself_again() {
    let h = heap();
    let l = locator();
    let mut found = l.find(&h, ASPECT, &mut go).unwrap();
    // The copies stopped while nobody read them; a read a minute later must not use them.
    h.live.borrow_mut().clear();
    let later = Instant::now() + Duration::from_secs(60);
    assert!(found.read(&h, &l, ASPECT, later).is_err());
    assert!(found.read(&h, &l, ASPECT, later + Duration::from_millis(8)).is_err());
    // When they jitter again, they are live again.
    h.live.borrow_mut().extend([POOL + 0x260, COPY + 0x40]);
    assert!(found.read(&h, &l, ASPECT, later + Duration::from_millis(16)).is_ok());
}

// R4 run 1 on a test PC (2026-10-21) failed with one text for three causes, so its cause stayed
// unknown. Each miss now names its reason, and `inspect` shows the state of each kept copy.
#[test]
fn each_miss_names_its_reason() {
    let h = heap();
    h.live.borrow_mut().push(OTHER + 0x300);
    let l = locator();
    let mut found = l.find(&h, ASPECT, &mut go).unwrap();
    let t = Instant::now();
    let (copies, v) = found.inspect(&h, &l, ASPECT, t);
    assert!(v.is_ok() && copies.len() == 3 && copies.iter().all(|c| c.value.is_some() && c.live));
    // Two copies move to another camera: one against two is a majority, one against one a tie.
    for (block, at) in [(POOL, 0x260), (COPY, 0x40)] { h.blocks[&block].borrow_mut()[at..at + 64].copy_from_slice(&matrix(30.0, ASPECT, 0.059)); }
    h.blocks[&COPY].borrow_mut()[0x40..0x80].copy_from_slice(&[0u8; 64]);
    let (copies, v) = found.inspect(&h, &l, ASPECT, t);
    assert_eq!(v, Err(Miss::Tie));
    assert!(copies.iter().any(|c| c.address == COPY + 0x40 && c.value.is_none() && !c.live), "the zeroed copy fails the checks");
    // The jitter stops: the copies still pass the checks but are stale after FRESH.
    h.live.borrow_mut().clear();
    assert_eq!(found.inspect(&h, &l, ASPECT, t + FRESH + Duration::from_millis(20)).1, Err(Miss::Stale));
    // Another view aspect: no copy passes the checks.
    assert_eq!(found.inspect(&h, &l, 1.5, t).1, Err(Miss::Gone));
    assert!(found.read(&h, &l, 1.5, t).unwrap_err().to_string().contains(Miss::Gone.text()));
}

#[test]
fn no_camera_without_a_match_a_scan_or_time() {
    let l = locator();
    assert!(matches!(l.find(&heap(), 1.5, &mut go), Err(Error::Unavailable(_))), "another window aspect: nothing plausible");
    assert!(matches!(l.find(&Heap { scan: false, ..heap() }, ASPECT, &mut go), Err(Error::Unavailable(_))), "the default adapter refuses a scan");
    assert!(l.find(&heap(), ASPECT, &mut |_| false).is_err(), "a stop ends the search");
}

#[test]
fn the_recipe_holds_limits_only() {
    let refuse = |change: serde_json::Value| {
        let mut v = serde_json::to_value(locator()).unwrap();
        for (k, x) in change.as_object().unwrap() { v[k] = x.clone(); }
        serde_json::from_value::<Locator>(v).map_err(|_| Error::Invalid("serde".into())).and_then(|l| l.check()).unwrap_err()
    };
    refuse(serde_json::json!({ "near": [0.0, 2.0] }));
    refuse(serde_json::json!({ "fov": [150.0, 5.0] }));
    refuse(serde_json::json!({ "aspect_tolerance": 0.5 }));
    refuse(serde_json::json!({ "pattern": "48 8B" }));
    assert!(locator().check().is_ok());
}
