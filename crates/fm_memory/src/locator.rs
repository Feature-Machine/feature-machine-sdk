// camera-locator-v1 (plan 2026-10-20-camera-map-updates, section 3 layer 2). MSFS has no stable
// anchor for its camera (no static owner, scrambled RTTI), so a signed recipe holds only limits
// and the host scans the private read-write memory for projections that pass checks that do not
// depend on the build. Only copies whose frame jitter changes are kept (proved live); each read
// takes the majority value of the copies that are still live. A recipe has no addresses or code.
use crate::{projection, valid_id, Error, Memory, Scalar};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Upper bounds keep a scan finite on any process (R2: 72 plausible blocks in 8.3 GB).
const MAX_CANDIDATES: usize = 4096;
const KEEP: usize = 16;
const SAMPLES: usize = 8;
const SAMPLE_GAP: Duration = Duration::from_millis(10);
/// MSFS keeps each view for this frame and the previous one, so during a zoom the copies split
/// in two equal groups (live 2026-10-20: 16 at 44.03 and 16 at 44.51 degrees). Copies within this
/// relative step of tan(vfov/2) are one camera; a slow game at 20 frames per second steps about 1%.
const FRAME_STEP: f32 = 0.05;
/// A camera whose jitter keeps one value longer than this is stale (a freed view, a closed
/// flight). A running game draws a frame at least each 50 ms, also with frame generation.
pub const FRESH: Duration = Duration::from_millis(250);

/// Proof that a camera source is live: its TAA or upscaler jitter (m[2], m[6]) changed between
/// two reads at most `FRESH` apart, within `FRESH` of now. A longer gap between reads proves
/// nothing, so it resets the proof (review rounds 1 and 2: stale copies and resumed sources).
/// Without anti-aliasing nothing jitters, so there is no camera: a wrong camera must never reach
/// the shader (plan section 1, done item 3). The exact map and the locator use the same proof.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live { last: Option<([f32; 2], Instant)>, changed: Option<Instant> }

impl Live {
    /// Records the jitter of `v` read at `now`; true while the source is proved live.
    pub fn see(&mut self, v: &Scalar, now: Instant) -> bool {
        let Scalar::Camera { jitter, .. } = v else { return false };
        match self.last {
            Some((j, at)) if now.saturating_duration_since(at) <= FRESH => if j != *jitter { self.changed = Some(now) },
            _ => self.changed = None,
        }
        self.last = Some((*jitter, now));
        self.proved(now)
    }

    /// True while the last jitter change is at most `FRESH` before `now`, with no new sample:
    /// a caller that keeps an earlier value (the host across a reused slot) checks it here.
    pub fn proved(&self, now: Instant) -> bool { self.changed.is_some_and(|c| now.saturating_duration_since(c) <= FRESH) }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Locator {
    pub schema: u32, pub id: String, pub version: String, pub game: String, pub module: String,
    /// Plausible near plane in metres and vertical FOV in degrees, both inclusive.
    pub near: [f32; 2], pub fov: [f32; 2],
    /// Relative tolerance of the projection aspect (m[5]/m[0]) against the window client aspect.
    pub aspect_tolerance: f32,
}

impl Locator {
    pub fn check(&self) -> Result<(), Error> {
        let ok = self.schema == 1 && valid_id(&self.id) && valid_id(&self.game) && semver::Version::parse(&self.version).is_ok()
            && !self.module.is_empty() && self.module.len() <= 128 && !self.module.contains(['/', '\\', ':'])
            && self.near[0] > 0.0 && self.near[0] < self.near[1] && self.near[1] <= 10.0
            && self.fov[0] >= 1.0 && self.fov[0] < self.fov[1] && self.fov[1] <= 170.0
            && self.aspect_tolerance > 0.0 && self.aspect_tolerance <= 0.05;
        if ok { Ok(()) } else { Err(Error::Invalid("camera locator metadata".into())) }
    }

    /// The build-independent limits: near and FOV, and the aspect of the window that the game
    /// draws to. A shadow, cube or menu view fails at least one of them. The host applies them to
    /// exact map values too, so both layers give only values that pass the same checks.
    pub fn admits(&self, v: &Scalar, aspect: f32) -> bool {
        let Scalar::Camera { near, tan_half_v, aspect: a, .. } = *v else { return false };
        let fov = (2.0 * tan_half_v.atan()).to_degrees();
        (self.near[0]..=self.near[1]).contains(&near) && (self.fov[0]..=self.fov[1]).contains(&fov)
            && aspect > 0.0 && (a / aspect - 1.0).abs() <= self.aspect_tolerance
    }

    fn accept(&self, b: &[u8], aspect: f32) -> Option<Scalar> { projection(b).filter(|v| self.admits(v, aspect)) }

    /// Scan, then sample the candidates `SAMPLES` times. `wait` sleeps and returns false when the
    /// caller stops. A candidate must pass every sample and change its jitter between samples;
    /// the majority value of those live copies is the camera.
    pub fn find<M: Memory>(&self, memory: &M, aspect: f32, wait: &mut dyn FnMut(Duration) -> bool) -> Result<Located, Error> {
        self.check()?;
        let mut found = Vec::new();
        let mut stopped = false;
        memory.scan(&mut |at, bytes| {
            if !wait(Duration::ZERO) { stopped = true; return false; }
            if bytes.len() >= 64 {
                for o in (0..=bytes.len() - 64).step_by(4) {
                    if bytes[o + 56..o + 60] == 1f32.to_le_bytes() && self.accept(&bytes[o..o + 64], aspect).is_some() { found.push(at + o as u64); }
                }
            }
            found.len() < MAX_CANDIDATES
        })?;
        if stopped { return Err(Error::Unavailable("stopped".into())); }
        // Each candidate keeps its own Live proof with the real read times (review round 3), and
        // its last value and verdict. None marks a candidate that failed a sample; it never comes back.
        let mut samples: Vec<Option<(Live, Scalar, bool)>> = vec![Some((Live::default(), Scalar::F32(0.0), false)); found.len()];
        for round in 0..SAMPLES {
            if round > 0 && !wait(SAMPLE_GAP) { return Err(Error::Unavailable("stopped".into())); }
            for (a, s) in found.iter().zip(samples.iter_mut()) {
                let Some((live, last, fresh)) = s.as_mut() else { continue };
                match memory.read_heap(*a, 64).ok().and_then(|b| self.accept(&b, aspect)) {
                    Some(v) => { (*fresh, *last) = (live.see(&v, Instant::now()), v); }
                    None => *s = None,
                }
            }
        }
        let live: Vec<(u64, Live, Scalar)> = found.iter().zip(samples).filter_map(|(a, s)| s.filter(|s| s.2).map(|(l, v, _)| (*a, l, v))).collect();
        let group = majority(live.iter().map(|(_, _, v)| *v))
            .ok_or_else(|| Error::Unavailable("no live camera found (no plausible copy changes its frame jitter; anti-aliasing off?)".into()))?;
        let copies = live.into_iter().filter(|(_, _, v)| close(*v, group)).take(KEEP).map(|(a, l, _)| (a, l)).collect();
        Ok(Located { copies })
    }
}

/// The kept copies of one session. Heap addresses change per session, so this is never stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Located { copies: Vec<(u64, Live)> }

impl Located {
    pub fn len(&self) -> usize { self.copies.len() }
    pub fn is_empty(&self) -> bool { self.copies.is_empty() }

    /// The majority value of the kept copies that pass the checks at `now` and are still live. A
    /// tie or no such copy is an error, so the caller clears the camera (and scans again later)
    /// rather than guess.
    pub fn read<M: Memory>(&mut self, memory: &M, locator: &Locator, aspect: f32, now: Instant) -> Result<Scalar, Error> {
        self.inspect(memory, locator, aspect, now).1.map_err(|m| Error::Unavailable(m.text().into()))
    }

    /// `read` with the state of each kept copy and the reason of a miss. The creator tools show
    /// both, so a failed check names its cause (R4 run 1 on a test PC gave one text for three
    /// causes, and its cause stayed unknown). The result is the same as `read`.
    pub fn inspect<M: Memory>(&mut self, memory: &M, locator: &Locator, aspect: f32, now: Instant) -> (Vec<CopyRead>, Result<Scalar, Miss>) {
        let copies: Vec<CopyRead> = self.copies.iter_mut().map(|(a, live)| {
            let value = memory.read_heap(*a, 64).ok().and_then(|b| locator.accept(&b, aspect));
            CopyRead { address: *a, value, live: value.is_some_and(|v| live.see(&v, now)) }
        }).collect();
        let result = if copies.iter().all(|c| c.value.is_none()) { Err(Miss::Gone) }
            else if !copies.iter().any(|c| c.live) { Err(Miss::Stale) }
            else { majority(copies.iter().filter(|c| c.live).filter_map(|c| c.value)).ok_or(Miss::Tie) };
        (copies, result)
    }
}

/// One kept copy at one read: its address, its value when it passes the checks, and its proof.
#[derive(Clone, Debug, PartialEq)]
pub struct CopyRead { pub address: u64, pub value: Option<Scalar>, pub live: bool }

/// Why a read of the kept copies gives no camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Miss {
    /// No kept copy holds a projection that passes the checks (freed, reused or another view).
    Gone,
    /// Some copies pass the checks, but none changed its jitter within `FRESH`.
    Stale,
    /// The live copies split between two cameras with equal support.
    Tie,
}

impl Miss {
    pub fn key(self) -> &'static str { match self { Self::Gone => "gone", Self::Stale => "stale", Self::Tie => "tie" } }
    pub fn text(self) -> &'static str {
        match self {
            Self::Gone => "no located camera copy passes the checks",
            Self::Stale => "the located camera copies are stale (no jitter change)",
            Self::Tie => "the live camera copies split between two cameras",
        }
    }
}

/// Equal near, and tan(vfov/2) within `step` of the larger value: float noise for copies of one
/// matrix, `FRAME_STEP` for one camera over two frames. Symmetric, so the grouping does not
/// depend on which copy comes first in memory (review 2026-10-20).
fn within(a: Scalar, b: Scalar, step: f32) -> bool {
    match (a, b) {
        (Scalar::Camera { near: n1, tan_half_v: t1, .. }, Scalar::Camera { near: n2, tan_half_v: t2, .. }) => (n1 - n2).abs() <= 1e-5 && (t1 - t2).abs() <= step * t1.abs().max(t2.abs()),
        _ => false,
    }
}
fn close(a: Scalar, b: Scalar) -> bool { within(a, b, FRAME_STEP) }

/// Groups by `same` and returns the largest, in first-seen order on equal counts; the bool is
/// true when the two largest groups tie.
fn groups(values: impl Iterator<Item = Scalar>, same: impl Fn(Scalar, Scalar) -> bool) -> Option<(Scalar, bool)> {
    let mut g: Vec<(Scalar, usize)> = Vec::new();
    for v in values {
        match g.iter_mut().find(|(s, _)| same(*s, v)) { Some((_, n)) => *n += 1, None => g.push((v, 1)) }
    }
    g.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    g.first().map(|(v, n)| (*v, g.get(1).is_some_and(|(_, m)| m == n)))
}

/// The camera that most copies agree on (copies one frame step apart are one camera), and as
/// its value the largest exact group of that camera; on an equal split of its two frames the
/// first one (at most one frame old). None when there is no copy or two cameras tie.
fn majority(values: impl Iterator<Item = Scalar>) -> Option<Scalar> {
    let values: Vec<Scalar> = values.collect();
    let (camera, tie) = groups(values.iter().copied(), close)?;
    if tie { return None; }
    groups(values.into_iter().filter(|v| close(camera, *v)), |a, b| within(a, b, 1e-4)).map(|(v, _)| v)
}
