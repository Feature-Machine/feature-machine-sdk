// Shared memory-map contract. Resolution and leases live here so every host uses the same
// build checks, ownership and restoration rules. OS access belongs to fm_platform.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(String), Unavailable(String), Refused, Busy, Conflict,
    /// A projection slot holds a well-formed view of another kind (finite far plane or
    /// orthographic): the game reuses the slot for other views in a frame (live 2026-10-21), so
    /// this read has no camera sample, but the slot is not broken. Callers may keep a fresh value.
    OtherView,
    /// An adapter may report exact intermediate bytes only when its operation proves them.
    /// A later read alone cannot distinguish a partial write from a foreign writer.
    PartialWrite { detail: String, bytes: Vec<u8> },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for Error {}

/// Process birth identity and module digest prevent a handle from surviving a new game or build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleId { pub game: String, pub module: String, pub sha256: String, pub session: u64 }

pub struct Image { pub base: u64, pub bytes: Vec<u8>, pub id: ModuleId, pub readable: Vec<std::ops::Range<usize>> }

impl Image {
    fn contains(&self, at: usize, size: usize) -> bool {
        at.checked_add(size).is_some_and(|end| self.readable.iter().any(|r| at >= r.start && end <= r.end))
    }
}

/// Hosts supply checked access. A write adapter must enforce its qualified execution thread;
/// a read-only adapter always returns false and refuses write, regardless of map access rights.
pub trait Memory {
    fn identity(&self) -> Result<ModuleId, Error>;
    fn read(&self, address: u64, size: usize) -> Result<Vec<u8>, Error>;
    fn can_write(&self) -> bool;
    fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), Error>;
    /// Call a resolved native change handler as `fn(object)`. Only an adapter at its qualified
    /// execution point may run it; the function always comes from a type-checked vtable.
    fn call(&mut self, _function: u64, _object: u64) -> Result<(), Error> { Err(Error::Unavailable("no qualified native call".into())) }
    /// A small read outside the main image, only at an address that a pointer step gave
    /// (camera-projection-v1: the game camera lives on the heap). An adapter that cannot read
    /// there without a fault refuses; `read` keeps its main-image limit for every other symbol.
    fn read_heap(&self, _address: u64, _size: usize) -> Result<Vec<u8>, Error> { Err(Error::Unavailable("no heap read".into())) }
    /// camera-locator-v1 only: give the committed private read-write memory of the process to
    /// `visit` in chunks of at most 1 MiB, read-only, until it returns false. A page that the
    /// OS refuses is skipped. Adapters without this access refuse (the default).
    fn scan(&self, _visit: &mut dyn FnMut(u64, &[u8]) -> bool) -> Result<(), Error> { Err(Error::Unavailable("no memory scan".into())) }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind { F32, I32, Bool, Projection }

impl Kind {
    fn size(self) -> usize { match self { Self::Bool => 1, Self::Projection => 64, _ => 4 } }
    fn decode(self, b: &[u8]) -> Result<Scalar, Error> {
        let invalid = || Error::Unavailable("invalid scalar representation".into());
        match self {
            Self::Projection => projection(b).ok_or_else(|| if other_view(b) { Error::OtherView } else { Error::Unavailable("not a reversed-Z perspective projection".into()) }),
            Self::F32 => {
                let v = f32::from_le_bytes(b.try_into().map_err(|_| invalid())?);
                if v.is_finite() { Ok(Scalar::F32(v)) } else { Err(invalid()) }
            }
            Self::I32 => Ok(Scalar::I32(i32::from_le_bytes(b.try_into().map_err(|_| invalid())?))),
            Self::Bool => match b { [0] => Ok(Scalar::Bool(false)), [1] => Ok(Scalar::Bool(true)), _ => Err(invalid()) },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// `aspect` of a camera is m[5]/m[0] (width over height of its view), for the window check;
/// `jitter` is m[2] and m[6], which change each frame on a live copy (locator::Live).
pub enum Scalar { F32(f32), I32(i32), Bool(bool), Camera { near: f32, tan_half_v: f32, aspect: f32, jitter: [f32; 2] } }

impl Scalar {
    fn kind(self) -> Kind { match self { Self::F32(_) => Kind::F32, Self::I32(_) => Kind::I32, Self::Bool(_) => Kind::Bool, Self::Camera { .. } => Kind::Projection } }
    /// The value that min and max limit; for a camera, the vertical FOV in degrees.
    fn number(self) -> f64 {
        match self {
            Self::F32(v) => v as f64, Self::I32(v) => v as f64, Self::Bool(v) => u8::from(v) as f64,
            Self::Camera { tan_half_v, .. } => (2.0 * (tan_half_v as f64).atan()).to_degrees(),
        }
    }
    /// Only writable kinds reach this (check refuses a writable projection).
    fn bytes(self) -> Vec<u8> {
        match self { Self::F32(v) => v.to_le_bytes().to_vec(), Self::I32(v) => v.to_le_bytes().to_vec(), Self::Bool(v) => vec![u8::from(v)], Self::Camera { .. } => vec![] }
    }
}

/// A view projection as MSFS 2024 stores it (1.9.20.0, plan 7.5): column layout, reversed-Z with
/// infinite far, so m[11] is the near plane and m[14] is 1. m[2] and m[6] carry the TAA jitter.
/// Any other shape (a cube, shadow or menu view in a reused slot) is not a camera.
fn projection(b: &[u8]) -> Option<Scalar> {
    let m: Vec<f32> = b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    let small = |i: usize, limit: f32| m[i].abs() < limit;
    let shape = m.len() == 16 && m.iter().all(|v| v.is_finite()) && m[0] > 0.0 && m[5] > 0.0 && m[11] > 0.0 && m[11] < 10.0
        && (m[14] - 1.0).abs() < 1e-3 && [1, 3, 4, 7, 8, 9, 12, 13, 15].iter().all(|&i| small(i, 1e-4))
        && small(2, 0.02) && small(6, 0.02) && small(10, 1e-3);
    shape.then(|| Scalar::Camera { near: m[11], tan_half_v: 1.0 / m[5], aspect: m[5] / m[0], jitter: [m[2], m[6]] })
}

/// A shape check for the other views seen in the exact map slot (2026-10-21, `drops/` traces),
/// in the same column layout: a perspective with a finite far plane (m[14] 1, m[10] < 0 and
/// m[11] > 0; MSFS near 0.1 m, far 1 m) or an orthographic view (m[14] 0, m[15] 1, depth scale
/// m[10] not 0). It does not prove the bytes are a view; it only names these shapes, so other
/// content (a moved or freed slot) stays an error. A caller keeps a camera at most briefly.
fn other_view(b: &[u8]) -> bool {
    let m: Vec<f32> = b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    let small = |i: usize| m[i].abs() < 1e-4;
    let near = |i: usize, v: f32| (m[i] - v).abs() < 1e-3;
    m.len() == 16 && m.iter().all(|v| v.is_finite()) && m[0] > 0.0 && m[5] > 0.0
        && [1, 3, 4, 7, 8, 9, 12, 13].iter().all(|&i| small(i))
        && ((near(14, 1.0) && small(15) && m[10] <= -1e-3 && m[11] > 0.0) || (small(14) && near(15, 1.0) && m[10].abs() > 1e-6))
}

mod locator;
pub use locator::{CopyRead, Live, Located, Locator, Miss, FRESH};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access { Read, Write }

/// The first map contract supports anchored scalars only. New layouts need a new contract,
/// not executable expressions or a callback address supplied by the map. camera-projection-v1
/// adds `rip` (the rel32 operand at this pattern offset, which ends its instruction, gives the
/// static address) and `steps` (at each read: take the 8-byte pointer there, add the step).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Symbol {
    pub name: String, pub pattern: String, pub offset: i64, pub kind: Kind, pub access: Access, pub min: f64, pub max: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_guard: Option<PointerGuard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_guard: Option<ClassGuard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setter: Option<Setter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rip: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<i64>,
}

/// Host-implemented setter contracts. `mirrored_notify_v1` is the observed MSFS tweakable
/// write: store value and f32 mirror, then call vtable slot `notify_slot` with the object if
/// the value changed. Offsets are relative to the value. The map never supplies code.
/// Numbers have a native clamp (min and max of the value kind); a bool has none (1.9.20.0).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Contract { MirroredNotifyV1 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setter {
    pub contract: Contract,
    pub mirror: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clamp_min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clamp_max: Option<i64>,
    pub notify_slot: u32,
}

/// A build-specific vtable identity relative to the unique pattern. This is a structural check,
/// never a fallback address or a function that a map can execute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerGuard { pub offset: i64, pub rva: u64 }

/// A build-independent class check (mirrored-setter-v2, plan 2026-10-23-write-maps-survive-
/// updates): the 8-byte pointer at `offset` from the pattern must point into read-only module
/// data, and at least `members` aligned copies of it in the image must carry `member` (a masked
/// pattern at the same place as the symbol pattern). So a lone or foreign object never passes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassGuard { pub offset: i64, pub member: String, pub members: u32 }

/// `sha256` is the exe digest of an exact map. A map without it is build-independent: each of
/// its symbols needs a class guard, and a runtime admits its writers only after `Runtime::watch`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Map {
    pub schema: u32, pub id: String, pub version: String, pub game: String, pub module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub symbols: Vec<Symbol>,
}

pub fn valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !s.starts_with('-') && !s.ends_with('-')
}

/// A bounded masked pattern with at least four fixed bytes. Wildcards never match unread memory.
fn pattern(text: &str) -> Result<Vec<Option<u8>>, Error> {
    let p: Result<Vec<_>, _> = text.split_whitespace().map(|s| {
        if s == "??" { Ok(None) } else if s.len() == 2 { u8::from_str_radix(s, 16).map(Some).map_err(|_| Error::Invalid("pattern byte".into())) }
        else { Err(Error::Invalid("pattern byte".into())) }
    }).collect();
    let p = p?;
    if !(4..=256).contains(&p.len()) || p.iter().flatten().count() < 4 { return Err(Error::Invalid("pattern needs 4 fixed bytes and at most 256 bytes".into())); }
    Ok(p)
}

fn matches(p: &[Option<u8>], b: &[u8]) -> bool { p.len() == b.len() && p.iter().zip(b).all(|(p, b)| p.is_none_or(|v| v == *b)) }
fn overlap(a: u64, an: usize, b: u64, bn: usize) -> bool { a < b.saturating_add(bn as u64) && b < a.saturating_add(an as u64) }

/// Use the longest fixed run as a search seed. memmem avoids a full masked comparison at each
/// byte in a large module; final checks still prove the complete pattern and readable region.
fn candidates<'a>(p: &'a [Option<u8>], image: &'a Image) -> impl Iterator<Item = usize> + 'a {
    let mut best = (0, vec![]);
    let mut at = 0;
    while at < p.len() {
        if p[at].is_none() { at += 1; continue; }
        let start = at;
        let mut run = vec![];
        while at < p.len() && p[at].is_some() { run.push(p[at].unwrap()); at += 1; }
        if run.len() > best.1.len() { best = (start, run); }
    }
    let (offset, seed) = best;
    let finder = memchr::memmem::Finder::new(&seed).into_owned();
    let mut from = 0;
    std::iter::from_fn(move || loop {
        let hit = finder.find(image.bytes.get(from..)?).map(|i| from + i)?;
        from = hit + 1;
        let Some(start) = hit.checked_sub(offset) else { continue };
        if image.contains(start, p.len()) && image.bytes.get(start..start + p.len()).is_some_and(|b| matches(p, b)) { return Some(start); }
    })
}

/// The written span covers value (`size` bytes) and the f32 mirror, so one exact range carries
/// ownership and restore. Bytes beside a 1-byte bool value stay outside the span.
fn span(s: &Setter, size: usize) -> (i64, usize) {
    let start = s.mirror.min(0);
    (start, ((s.mirror + 4).max(size as i64) - start) as usize)
}

#[derive(Clone)]
struct Native { span: u64, len: usize, value_at: usize, mirror_at: usize, clamp: Option<(u64, u64)>, object: u64, function: u64 }

/// `proved`: a writer may acquire it. A v2 symbol starts unproved; `Runtime::watch` proves it
/// after `WATCH_MS` of consistent reads (`watch` holds the first time and the read count).
/// `found`: where a v2 symbol was found on this exe, for its saved result (`Runtime::proof`).
/// `failed`: why the last watch read failed, for the status of a writer that waits.
#[derive(Clone)]
struct Resolved { symbol: Symbol, address: u64, guard: u64, pattern: Vec<Option<u8>>, type_guard: Option<(u64, [u8; 8])>, native: Option<Native>, proved: bool, watch: Option<(u64, u32)>, found: Option<Found>, failed: Option<String> }

/// The saved result of a v2 map proved on one exe (plan 2026-10-23, owner answer 2: save per
/// build). It only skips the pattern search and the watch at the next start on that exe:
/// `Map::resolve_proved` checks each pattern, vtable, class, handler and setter state again,
/// and a record that differs is refused. It holds RVAs only, never code or an address to call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof { pub exe: String, pub map: String, pub version: String, pub file: String, pub symbols: BTreeMap<String, Found> }

/// One symbol of a `Proof`: the RVA of its name pattern and of its class vtable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Found { pub pattern: u64, pub vtable: u64 }

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

/// A real exe SHA-256 (64 lowercase hex). A reader that cannot hash the exe gives "" (Microsoft
/// Store build): such a start never saves or uses a saved result (review s3 r1).
fn exe_digest(s: &str) -> bool { s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }

impl Resolved {
    /// Storage a writer owns: the setter span, or the scalar itself.
    fn storage(&self) -> (u64, usize) { self.native.as_ref().map_or((self.address, self.symbol.kind.size()), |n| (n.span, n.len)) }
}

/// One map resolved in one module session. Its identity is (`map_id`, `version`, `digest`); the
/// digest is the verified SHA-256 of the signed map file, which the caller gives (a parsed `Map`
/// has no stable bytes to hash). `Runtime::add_maps` compares identities by map id.
pub struct ResolvedMap { id: ModuleId, map_id: String, version: String, digest: [u8; 32], symbols: BTreeMap<String, Resolved> }

impl ResolvedMap {
    pub fn map_id(&self) -> &str { &self.map_id }
}

/// The identity of a map in a runtime: its version and file digest, by map id.
type Identity = (String, [u8; 32]);

/// The map ids of one `Runtime::add_maps` batch: added now, or already there with the same identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Added { pub new: Vec<String>, pub present: Vec<String> }

impl Map {
    pub fn check(&self) -> Result<(), Error> {
        let mut names = BTreeSet::new();
        if self.schema != 1 || !valid_id(&self.id) || !valid_id(&self.game) || semver::Version::parse(&self.version).is_err()
            || self.module.is_empty() || self.module.len() > 128 || self.module.contains(['/', '\\', ':'])
            || self.sha256.as_ref().is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
            || !(1..=256).contains(&self.symbols.len()) { return Err(Error::Invalid("map metadata".into())); }
        for s in &self.symbols {
            if !valid_id(&s.name) || !names.insert(&s.name) || !s.min.is_finite() || !s.max.is_finite() || s.min > s.max {
                return Err(Error::Invalid("symbol metadata".into()));
            }
            let p = pattern(&s.pattern)?;
            // A build-independent map proves each object by its class, never by a build RVA.
            if let Some(class) = &s.class_guard {
                if s.type_guard.is_some() || !(-4096..=4096).contains(&class.offset) || class.offset % 8 != 0
                    || !(2..=1_000_000).contains(&class.members) || pattern(&class.member).is_err() {
                    return Err(Error::Invalid("class guard contract".into()));
                }
            }
            if self.sha256.is_none() && (s.class_guard.is_none() || s.rip.is_some() || !s.steps.is_empty() || s.kind == Kind::Projection) {
                return Err(Error::Invalid("a map without an exe digest needs a class guard on each symbol".into()));
            }
            // Pointer steps and the projection kind are read-only, bounded and anchored in code.
            let pointer = s.rip.is_some() || !s.steps.is_empty() || s.kind == Kind::Projection;
            if pointer && (s.access != Access::Read || s.setter.is_some() || s.type_guard.is_some() || s.steps.len() > 8
                || s.rip.is_none_or(|r| r < 0 || r as usize + 4 > p.len()) || s.steps.iter().any(|o| !(0..=0x10_0000).contains(o))) {
                return Err(Error::Invalid("pointer symbol contract".into()));
            }
            // Each kind follows the setter and handler disassembled on 1.9.20.0 (plan 11): f32
            // and i32 have a native clamp, a bool has none. The mirror never aliases the value.
            if let Some(setter) = &s.setter {
                let near = |o: i64| (-4096..=4096).contains(&o);
                let clamp: Vec<i64> = setter.clamp_min.into_iter().chain(setter.clamp_max).collect();
                if s.access != Access::Write || (s.type_guard.is_none() && s.class_guard.is_none()) || setter.notify_slot > 63
                    || clamp.len() != if s.kind == Kind::Bool { 0 } else { 2 }
                    || !clamp.iter().chain([&setter.mirror]).all(|o| near(*o)) || setter.mirror.abs() < 4 || span(setter, s.kind.size()).1 > 16 {
                    return Err(Error::Invalid("setter contract".into()));
                }
            }
        }
        Ok(())
    }

    /// `resolve_pinned` for a caller that has no verified file digest (creator tools, a runtime
    /// that never adds maps): the digest is all zeros. The host passes the digest of the signed
    /// file, so two sessions that pin other bytes of one map version give a `Conflict`.
    pub fn resolve(&self, image: &Image) -> Result<ResolvedMap, Error> { self.resolve_pinned(image, [0; 32]) }

    /// Resolution runs off the frame path. Every symbol must resolve once within this image;
    /// partial maps are refused so an activated package has one complete, compatible contract.
    pub fn resolve_pinned(&self, image: &Image, digest: [u8; 32]) -> Result<ResolvedMap, Error> { self.resolve_with(image, digest, None) }

    /// A v2 map from its saved result on this exe: no pattern search and no watch (its symbols
    /// start proved). All other checks of `resolve_pinned` run again, the class scan too. A record
    /// for another exe, map version or map file, an exe with no digest (Store build), or one that
    /// the image does not confirm, is Unavailable: the caller discards it and resolves again.
    pub fn resolve_proved(&self, image: &Image, digest: [u8; 32], proof: &Proof) -> Result<ResolvedMap, Error> {
        let names: BTreeSet<&str> = self.symbols.iter().map(|s| s.name.as_str()).collect();
        if self.sha256.is_some() || !exe_digest(&image.id.sha256) || proof.exe != image.id.sha256 || proof.map != self.id || proof.version != self.version || proof.file != hex(&digest)
            || proof.symbols.keys().map(String::as_str).collect::<BTreeSet<_>>() != names {
            return Err(Error::Unavailable("saved result is for another exe or map".into()));
        }
        self.resolve_with(image, digest, Some(proof))
    }

    fn resolve_with(&self, image: &Image, digest: [u8; 32], saved: Option<&Proof>) -> Result<ResolvedMap, Error> {
        self.check()?;
        if self.game != image.id.game || !self.module.eq_ignore_ascii_case(&image.id.module) || self.sha256.as_ref().is_some_and(|h| *h != image.id.sha256) {
            return Err(Error::Unavailable("module identity differs from the map".into()));
        }
        let (sections, mut classes) = (sections(image), Vec::new());
        let mut symbols = BTreeMap::new();
        for s in &self.symbols {
            let p = pattern(&s.pattern)?;
            let found = saved.map(|proof| proof.symbols[&s.name]);
            // The saved place must still hold the full pattern; else the one unique match.
            let at = match found {
                Some(f) => usize::try_from(f.pattern).ok().filter(|at| image.contains(*at, p.len()) && image.bytes.get(*at..*at + p.len()).is_some_and(|b| matches(&p, b)))
                    .ok_or_else(|| Error::Unavailable(format!("{}: saved result differs from the image", s.name)))?,
                None => {
                    let mut hits = candidates(&p, image);
                    let Some(at) = hits.next() else { return Err(Error::Unavailable(format!("{}: no match", s.name))) };
                    if hits.next().is_some() { return Err(Error::Unavailable(format!("{}: ambiguous pattern", s.name))); }
                    at
                }
            };
            let anchor = match s.rip {
                Some(r) => {
                    let o = at + r as usize;
                    let rel = i32::from_le_bytes(image.bytes[o..o + 4].try_into().unwrap());
                    (o as u64 + 4).checked_add_signed(rel as i64)
                }
                None => Some(at as u64),
            };
            let target = anchor.and_then(|a| a.checked_add_signed(s.offset)).ok_or_else(|| Error::Invalid("symbol offset overflow".into()))?;
            // A pointer symbol's static slot holds an 8-byte pointer; its value is read later.
            let size = if s.steps.is_empty() { s.kind.size() } else { 8 };
            let end = target.checked_add(size as u64).ok_or_else(|| Error::Invalid("symbol end overflow".into()))?;
            if end > image.bytes.len() as u64 || !image.contains(target as usize, size) || overlap(target, size, at as u64, p.len()) {
                return Err(Error::Invalid("symbol outside image or overlapping its guard".into()));
            }
            let address = image.base.checked_add(target).ok_or_else(|| Error::Invalid("address overflow".into()))?;
            let guard = image.base.checked_add(at as u64).ok_or_else(|| Error::Invalid("guard overflow".into()))?;
            if image.base.checked_add(image.bytes.len() as u64).is_none() { return Err(Error::Invalid("image end overflow".into())); }
            // A heap value can be absent at resolution (no flight yet), so only a static is checked.
            if s.steps.is_empty() {
                let v = s.kind.decode(&image.bytes[target as usize..end as usize])?;
                validate(s, v)?;
            }
            // The object's vtable: from the map RVA (v1, one build) or proved by its class (v2).
            let vtable = match (&s.type_guard, &s.class_guard) {
                (Some(check), None) => {
                    let at = (at as u64).checked_add_signed(check.offset).ok_or_else(|| Error::Invalid("type guard offset overflow".into()))?;
                    if at > usize::MAX as u64 || !image.contains(at as usize, 8) || check.rva >= image.bytes.len() as u64 {
                        return Err(Error::Invalid("type guard outside image".into()));
                    }
                    let expected = image.base.checked_add(check.rva).ok_or_else(|| Error::Invalid("type pointer overflow".into()))?.to_le_bytes();
                    if image.bytes.get(at as usize..at as usize + 8) != Some(expected.as_slice()) { return Err(Error::Unavailable("object type differs from the map".into())); }
                    Some((image.base.checked_add(at).ok_or_else(|| Error::Invalid("type guard address overflow".into()))?, expected, check.rva))
                }
                (None, Some(class)) => Some(class_vtable(image, &sections, class, s.kind, at, &mut classes, found.map(|f| f.vtable))?),
                _ => None,
            };
            let type_guard = vtable.map(|(object, expected, _)| (object, expected));
            let native = match (&s.setter, vtable) {
                (Some(setter), Some((object, _, rva))) => Some(native(image, &sections, setter, rva, s.kind.size(), target, address, object)?),
                _ => None,
            };
            // A v2 setter must look like the contract now: value in the native clamp, mirror equal.
            if let (Some(_), Some(n)) = (&s.class_guard, &native) { setter_state(image, s, n)?; }
            let proved = s.class_guard.is_none() || found.is_some();
            let found = s.class_guard.as_ref().and(vtable).map(|(_, _, vtable)| Found { pattern: at as u64, vtable });
            symbols.insert(format!("{}/{}", self.id, s.name), Resolved { symbol: s.clone(), address, guard, pattern: p, type_guard, native, proved, watch: None, found, failed: None });
        }
        for s in symbols.values().filter(|s| s.symbol.access == Access::Write) {
            if symbols.values().any(|other| overlaps_guard(s, other)) {
                return Err(Error::Invalid("writer overlaps an object guard".into()));
            }
        }
        Ok(ResolvedMap { id: image.id.clone(), map_id: self.id.clone(), version: self.version.clone(), digest, symbols })
    }
}

/// Resolve the setter span, native bounds and change handler inside this exact image. The handler
/// pointer is read from the guarded vtable, so a map can select a slot but never an address.
#[allow(clippy::too_many_arguments)]
fn native(image: &Image, sections: &[Section], setter: &Setter, vtable_rva: u64, size: usize, target: u64, address: u64, object: u64) -> Result<Native, Error> {
    let bad = || Error::Invalid("setter outside image".into());
    let (start, len) = span(setter, size);
    let local = |o: i64, n: usize| target.checked_add_signed(o).filter(|a| image.contains(*a as usize, n)).ok_or_else(bad);
    let first = local(start, len)?;
    for o in setter.clamp_min.into_iter().chain(setter.clamp_max) { local(o, size)?; }
    let slot = vtable_rva.checked_add(u64::from(setter.notify_slot) * 8).filter(|a| image.contains(*a as usize, 8)).ok_or_else(bad)?;
    let function = u64::from_le_bytes(image.bytes[slot as usize..slot as usize + 8].try_into().unwrap());
    if function < image.base || function >= image.base + image.bytes.len() as u64 { return Err(Error::Unavailable("change handler outside module".into())); }
    // With section headers, a handler must start in executable code (both contracts).
    if !sections.is_empty() && !section_at(sections, function - image.base).is_some_and(|s| s.exec && !s.write) {
        return Err(Error::Unavailable("change handler outside module code".into()));
    }
    let at = |o: i64| address.checked_add_signed(o).ok_or_else(bad);
    let clamp = match (setter.clamp_min, setter.clamp_max) { (Some(min), Some(max)) => Some((at(min)?, at(max)?)), _ => None };
    Ok(Native { span: image.base + first, len, value_at: (-start) as usize, mirror_at: (setter.mirror - start) as usize, clamp, object, function })
}

/// One PE section of the main image, by RVA, from the headers in the image bytes.
#[derive(Clone, Copy, Debug)]
struct Section { start: u64, end: u64, exec: bool, write: bool }

/// The section table of the image (empty when the bytes have no valid PE header, as in test
/// fixtures of v1 maps). Bounds are checked, so a damaged header gives no section.
fn sections(image: &Image) -> Vec<Section> {
    let b = &image.bytes;
    let u16_at = |o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes(x.try_into().unwrap()));
    let u32_at = |o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap()));
    let read = || -> Option<Vec<Section>> {
        if b.get(0..2)? != b"MZ" || !image.contains(0, 0x40) { return None; }
        let pe = u32_at(0x3c)? as usize;
        if b.get(pe..pe + 4)? != b"PE\0\0" { return None; }
        let (count, optional) = (u16_at(pe + 6)? as usize, u16_at(pe + 20)? as usize);
        let table = pe.checked_add(24 + optional)?;
        if count > 96 || !image.contains(table, count * 40) { return None; }
        (0..count).map(|i| {
            let o = table + 40 * i;
            let (size, rva, flags) = (u32_at(o + 8)? as u64, u32_at(o + 12)? as u64, u32_at(o + 36)?);
            Some(Section { start: rva, end: rva + size, exec: flags & 0x2000_0000 != 0, write: flags & 0x8000_0000 != 0 })
        }).collect()
    };
    read().unwrap_or_default()
}

fn section_at(sections: &[Section], rva: u64) -> Option<&Section> { sections.iter().find(|s| (s.start..s.end).contains(&rva)) }

/// The class proof of mirrored-setter-v2 (slice 0 on 1.9.22.0: 378 named tweak objects share one
/// vtable in `.rdata`). Returns (pointer address, pointer bytes, vtable RVA) like a type guard,
/// so the runtime checks it again at each access with `guard_matches`. `classes` holds the
/// classes that this map proved so far: symbols with the same member pattern and kind must share
/// one vtable (review r1), and a class proved once is not scanned again (one image pass each).
/// `saved`: the vtable RVA of a saved result on this exe; it must be the same.
fn class_vtable(image: &Image, sections: &[Section], class: &ClassGuard, kind: Kind, at: usize, classes: &mut Vec<(String, Kind, u32, [u8; 8])>, saved: Option<u64>) -> Result<(u64, [u8; 8], u64), Error> {
    let differs = |why: &str| Error::Unavailable(format!("object class differs from the map: {why}"));
    if sections.is_empty() { return Err(differs("no section table")); }
    let place = (at as u64).checked_add_signed(class.offset).filter(|p| image.contains(*p as usize, 8)).ok_or_else(|| Error::Invalid("class guard outside image".into()))?;
    let bytes: [u8; 8] = image.bytes[place as usize..place as usize + 8].try_into().unwrap();
    let rva = u64::from_le_bytes(bytes).checked_sub(image.base).ok_or_else(|| differs("vtable outside module"))?;
    if !section_at(sections, rva).is_some_and(|s| !s.exec && !s.write) || !image.contains(rva as usize, 8) { return Err(differs("vtable not in read-only data")); }
    let address = image.base.checked_add(place).ok_or_else(|| Error::Invalid("class guard address overflow".into()))?;
    // Each saved vtable is compared; the class population is counted again in all cases (review
    // s3 r1: a start can have fewer members on the same exe, and the scan is one memmem pass).
    if saved.is_some_and(|saved| saved != rva) { return Err(Error::Unavailable("saved result differs from the image: vtable".into())); }
    if let Some((_, _, members, vtable)) = classes.iter().find(|(m, k, _, _)| *m == class.member && *k == kind) {
        if *vtable != bytes { return Err(differs("symbols of one class have other vtables")); }
        if *members >= class.members { return Ok((address, bytes, rva)); }
    }
    // Members: aligned copies of the pointer whose bytes at the symbol's pattern place match.
    let member = pattern(&class.member)?;
    let finder = memchr::memmem::Finder::new(&bytes);
    let mut count = 0u32;
    for hit in finder.find_iter(&image.bytes) {
        if !(image.base + hit as u64).is_multiple_of(8) { continue; }
        let Some(start) = (hit as u64).checked_add_signed(-class.offset) else { continue };
        let start = start as usize;
        if image.contains(start, member.len()) && matches(&member, &image.bytes[start..start + member.len()]) {
            count += 1;
            if count >= class.members { break; }
        }
    }
    if count < class.members { return Err(differs(format!("{count} members, {} needed", class.members).as_str())); }
    classes.push((class.member.clone(), kind, class.members, bytes));
    Ok((address, bytes, rva))
}

/// A setter's state as the contract leaves it: value valid, mirror equal to the value as f32,
/// value inside the native clamp. `span` holds the setter bytes; `bound` reads a clamp value.
fn setter_consistent(s: &Symbol, n: &Native, span: &[u8], bound: impl Fn(u64) -> Result<f64, Error>) -> Result<(), Error> {
    let bad = |why: &str| Error::Unavailable(format!("setter state differs from the contract: {why}"));
    let value = s.kind.decode(span.get(n.value_at..n.value_at + s.kind.size()).ok_or_else(|| bad("span"))?)?;
    validate(s, value)?;
    let mirror = f32::from_le_bytes(span.get(n.mirror_at..n.mirror_at + 4).ok_or_else(|| bad("span"))?.try_into().unwrap());
    let expect = match value { Scalar::F32(v) => v, Scalar::I32(v) => v as f32, Scalar::Bool(v) => u8::from(v) as f32, Scalar::Camera { .. } => return Err(bad("camera")) };
    if mirror.to_bits() != expect.to_bits() { return Err(bad("mirror")); }
    if let Some((min, max)) = n.clamp {
        let (lo, hi, v) = (bound(min)?, bound(max)?, value.number());
        if !(lo.is_finite() && hi.is_finite() && lo <= v && v <= hi) { return Err(bad("clamp")); }
    }
    Ok(())
}

/// `setter_consistent` on the image bytes at resolution.
fn setter_state(image: &Image, s: &Symbol, n: &Native) -> Result<(), Error> {
    let local = |a: u64, len: usize| a.checked_sub(image.base).map(|r| r as usize).filter(|r| image.contains(*r, len)).map(|r| &image.bytes[r..r + len]);
    let span = local(n.span, n.len).ok_or_else(|| Error::Invalid("setter outside image".into()))?;
    setter_consistent(s, n, span, |a| local(a, s.kind.size()).ok_or_else(|| Error::Invalid("clamp outside image".into())).and_then(|b| s.kind.decode(b)).map(Scalar::number))
}

/// Decode the value from owned storage bytes (the scalar, or its place inside the setter span).
fn value_of(s: &Resolved, bytes: &[u8]) -> Result<Scalar, Error> {
    let at = s.native.as_ref().map_or(0, |n| n.value_at);
    s.symbol.kind.decode(bytes.get(at..at + s.symbol.kind.size()).ok_or(Error::Conflict)?)
}

/// New span bytes: value and its f32 mirror, as the native setter stores them (f32 bits,
/// (float)i32, bool 1.0 or 0.0). Bytes between them keep their current content.
fn compose(native: &Native, current: &[u8], value: Scalar) -> Vec<u8> {
    let mut bytes = current.to_vec();
    let mirror = match value { Scalar::F32(v) => v, Scalar::I32(v) => v as f32, Scalar::Bool(v) => u8::from(v) as f32, Scalar::Camera { .. } => 0.0 }; // a camera is never writable
    let value = value.bytes();
    bytes[native.value_at..native.value_at + value.len()].copy_from_slice(&value);
    bytes[native.mirror_at..native.mirror_at + 4].copy_from_slice(&mirror.to_le_bytes());
    bytes
}

fn validate(s: &Symbol, v: Scalar) -> Result<(), Error> {
    let n = v.number();
    if v.kind() != s.kind || !n.is_finite() || !(s.min..=s.max).contains(&n) { Err(Error::Invalid("scalar type or range".into())) } else { Ok(()) }
}

fn overlaps_guard(writer: &Resolved, other: &Resolved) -> bool {
    let (at, n) = writer.storage();
    overlap(at, n, other.guard, other.pattern.len()) || other.type_guard.is_some_and(|(address, _)| overlap(at, n, address, 8))
}

fn guard_matches<M: Memory>(memory: &M, s: &Resolved) -> bool {
    memory.read(s.guard, s.pattern.len()).is_ok_and(|b| matches(&s.pattern, &b))
        && s.type_guard.is_none_or(|(address, expected)| memory.read(address, 8).is_ok_and(|b| b == expected))
}

/// The read-only watch of a v2 symbol before its first writer: at least 5 consistent reads over
/// at least 10 s, all in a row. Agent choice (plan 2026-10-23 section 3): long enough to see a
/// wrong object change on its own, short against a flight; one failure starts it again.
pub const WATCH_MS: u64 = 10_000;
pub const WATCH_READS: u32 = 5;

/// `notify` marks a restored setter value whose change handler has not run yet.
struct Lease { owner: String, key: String, original: Vec<u8>, expected: Option<Vec<u8>>, failed: bool, notify: bool }
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

// Keep exact write outcomes for both mutation and restoration. An unproved intermediate result
// has no expected bytes, so a later cleanup cannot overwrite an unrelated change.
fn write_tracked<M: Memory>(memory: &mut M, lease: &mut Lease, address: u64, bytes: &[u8]) -> Result<(), Error> {
    let previous = lease.expected.take();
    let result = memory.write(address, bytes);
    let observed = memory.read(address, bytes.len());
    if observed.as_deref().is_ok_and(|b| b == bytes) {
        lease.expected = Some(bytes.to_vec());
        if result.is_ok() { return Ok(()); }
    } else if observed.as_ref().ok() == previous.as_ref() && previous.is_some() {
        lease.expected = previous;
    } else if let Err(Error::PartialWrite { bytes: known, .. }) = &result {
        if known.len() == bytes.len() && observed.as_ref().is_ok_and(|b| b == known) {
            lease.expected = Some(known.clone());
        }
    }
    lease.failed = true;
    Err(result.err().or_else(|| observed.err()).unwrap_or(Error::Conflict))
}

/// One read of a symbol before decode (`Runtime::read_raw`): the static slot and the address
/// after each pointer step, then the value bytes at the last address.
#[derive(Clone, Debug, PartialEq)]
pub struct RawRead { pub chain: Vec<u64>, pub bytes: Vec<u8> }

/// One runtime owns all maps for a module, so overlapping aliases share one writer lock.
/// Failed cleanup retains the lease and its range until process exit or a successful retry.
pub struct Runtime<M: Memory> {
    memory: M, id: ModuleId, maps: BTreeMap<String, Identity>, symbols: BTreeMap<String, Resolved>, leases: BTreeMap<u64, Lease>, stopped: BTreeSet<String>, disconnected: bool,
}

impl<M: Memory> Runtime<M> {
    /// An empty runtime plus `add_maps`, so a session start and a late admission use one check.
    pub fn new(memory: M, maps: Vec<ResolvedMap>) -> Result<Self, Error> {
        let id = memory.identity()?;
        let mut runtime = Self { memory, id, maps: BTreeMap::new(), symbols: BTreeMap::new(), leases: BTreeMap::new(), stopped: BTreeSet::new(), disconnected: false };
        runtime.add_maps(maps)?;
        Ok(runtime)
    }

    /// Adds a batch of maps resolved in this module session, all or nothing (plan 2026-10-19
    /// section 12, contract 2). A map id with the same identity is `present`; another version or
    /// digest, a duplicate symbol key, or a writer over a guard (new writers against all guards,
    /// existing writers against the new guards) is `Conflict`. Read aliases may overlap, as in
    /// `new`; `acquire` refuses overlapping leases. Leases and stopped owners do not change.
    pub fn add_maps(&mut self, maps: Vec<ResolvedMap>) -> Result<Added, Error> {
        let (mut added, mut ids, mut symbols) = (Added::default(), BTreeMap::new(), BTreeMap::new());
        for m in maps {
            if m.id != self.id { return Err(Error::Unavailable("maps from different module sessions".into())); }
            let identity = (m.version, m.digest);
            match self.maps.get(&m.map_id).or(ids.get(&m.map_id)) {
                Some(known) if *known == identity => { if !added.present.contains(&m.map_id) { added.present.push(m.map_id); } continue; }
                Some(_) => return Err(Error::Conflict),
                None => {}
            }
            for (key, symbol) in m.symbols {
                if self.symbols.contains_key(&key) || symbols.insert(key, symbol).is_some() { return Err(Error::Conflict); }
            }
            added.new.push(m.map_id.clone());
            ids.insert(m.map_id, identity);
        }
        let all = || self.symbols.values().chain(symbols.values());
        let writer = |s: &&Resolved| s.symbol.access == Access::Write;
        if symbols.values().filter(writer).any(|s| all().any(|other| overlaps_guard(s, other)))
            || self.symbols.values().filter(writer).any(|s| symbols.values().any(|other| overlaps_guard(s, other))) {
            return Err(Error::Conflict);
        }
        self.maps.extend(ids);
        self.symbols.extend(symbols);
        Ok(added)
    }

    /// Opens `owner` again after `stop` (an app link loss stops owners; `disconnect` is final).
    /// Refused while the owner holds a lease, also one whose restore failed, or after `disconnect`.
    /// Handles are never reused, so a handle from before the stop stays refused.
    pub fn admit(&mut self, owner: &str) -> Result<(), Error> {
        if owner.is_empty() || self.disconnected || self.holds(owner) { return Err(Error::Refused); }
        self.stopped.remove(owner);
        Ok(())
    }

    /// True while `owner` holds a lease, including one whose cleanup failed.
    pub fn holds(&self, owner: &str) -> bool { self.leases.values().any(|l| l.owner == owner) }
    pub fn leased(&self) -> bool { !self.leases.is_empty() }

    /// Host-owned access for adapter maintenance and test fault injection; never exposed in ABI.
    pub fn memory_mut(&mut self) -> &mut M { &mut self.memory }

    fn checked(&self, key: &str) -> Result<&Resolved, Error> {
        let s = self.symbols.get(key).ok_or_else(|| Error::Unavailable("symbol not in this session".into()))?;
        if self.memory.identity()? != self.id || !guard_matches(&self.memory, s) {
            return Err(Error::Unavailable("module session or object guard changed".into()));
        }
        Ok(s)
    }

    pub fn read(&self, key: &str) -> Result<Scalar, Error> { self.decode(key, &self.read_raw(key)?) }

    /// One read before decode: the address at each pointer step and the value bytes. `read` is
    /// `read_raw` then `decode`, so a creator tool (the trace option of `fm_pack map check`) can
    /// record the bytes of a read that fails, from the same path as the host.
    pub fn read_raw(&self, key: &str) -> Result<RawRead, Error> {
        let s = self.checked(key)?;
        // The static slot is in the image; each address after a pointer step is on the heap.
        let mut chain = vec![s.address];
        let read = |heap: bool, at: u64, n: usize| if heap { self.memory.read_heap(at, n) } else { self.memory.read(at, n) };
        for (i, step) in s.symbol.steps.iter().enumerate() {
            let b = read(i > 0, chain[i], 8)?;
            let pointer = u64::from_le_bytes(b.try_into().map_err(|_| Error::Unavailable("short pointer read".into()))?);
            chain.push(pointer.checked_add_signed(*step).filter(|_| pointer >= 0x10000).ok_or_else(|| Error::Unavailable("null or invalid pointer step".into()))?);
        }
        let bytes = read(!s.symbol.steps.is_empty(), chain[chain.len() - 1], s.symbol.kind.size())?;
        Ok(RawRead { chain, bytes })
    }

    /// The value of `raw` under the symbol's kind and range.
    pub fn decode(&self, key: &str, raw: &RawRead) -> Result<Scalar, Error> {
        let s = self.symbols.get(key).ok_or_else(|| Error::Unavailable("symbol not in this session".into()))?;
        let v = s.symbol.kind.decode(&raw.bytes)?;
        validate(&s.symbol, v)?;
        Ok(v)
    }

    pub fn acquire(&mut self, owner: &str, key: &str) -> Result<u64, Error> {
        if owner.is_empty() || self.disconnected || self.stopped.contains(owner) { return Err(Error::Refused); }
        let s = self.checked(key)?;
        if s.symbol.access != Access::Write { return Err(Error::Refused); }
        // The read-only watch ends by itself: Busy, so a writer tries again later (fm_feature_sdk
        // retries Busy and the host logs no line for it), as for a lease of another feature.
        if !s.proved { return Err(Error::Busy); }
        if !self.memory.can_write() { return Err(Error::Unavailable("no qualified execution point".into())); }
        let (at, n) = s.storage();
        if self.leases.values().any(|l| { let (o, on) = self.symbols[&l.key].storage(); overlap(at, n, o, on) }) { return Err(Error::Busy); }
        let original = self.memory.read(at, n)?;
        validate(&s.symbol, value_of(s, &original)?)?;
        let h = NEXT_HANDLE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| (v < i64::MAX as u64).then_some(v + 1)).map_err(|_| Error::Busy)?;
        self.leases.insert(h, Lease { owner: owner.into(), key: key.into(), expected: Some(original.clone()), original, failed: false, notify: false });
        Ok(h)
    }

    pub fn set(&mut self, owner: &str, handle: u64, value: Scalar) -> Result<(), Error> {
        let l = self.leases.get(&handle).ok_or(Error::Refused)?;
        if l.owner != owner || l.failed || self.stopped.contains(owner) { return Err(Error::Refused); }
        let s = self.checked(&l.key)?.clone();
        validate(&s.symbol, value)?;
        if !self.memory.can_write() { return Err(Error::Unavailable("not at the qualified execution point".into())); }
        let (address, n) = s.storage();
        let current = self.memory.read(address, n)?;
        if Some(&current) != l.expected.as_ref() { return Err(Error::Conflict); }
        let bytes = match &s.native {
            Some(native) => { self.native_bounds(native, &s.symbol, value)?; compose(native, &current, value) }
            None => value.bytes(),
        };
        let changed = value_of(&s, &current)? != value;
        let l = self.leases.get_mut(&handle).unwrap();
        let previous = l.expected.clone();
        let mut result = write_tracked(&mut self.memory, l, address, &bytes);
        // The change handler runs only after the exact span is proved. If it cannot run, the
        // previous span is restored at once: no reader keeps a value its owners never saw.
        if let (Ok(()), Some(native), true) = (&result, &s.native, changed) {
            if let Err(e) = self.memory.call(native.function, native.object) {
                if let Some(previous) = previous.clone() { let _ = write_tracked(&mut self.memory, l, address, &previous); }
                l.failed = true;
                result = Err(e);
            } else {
                // The handler is part of the write and may rewrite the span (MSFS slot 2 copies
                // the value into the mirror). Its result is the state that cleanup expects. A
                // handler that changed the value itself is not this contract: refuse more writes.
                match self.memory.read(address, n) {
                    Ok(after) => {
                        let kept = value_of(&s, &after).is_ok_and(|v| v == value);
                        l.expected = Some(after);
                        if !kept { l.failed = true; result = Err(Error::Conflict); }
                    }
                    Err(e) => { l.expected = None; l.failed = true; result = Err(e); }
                }
            }
        }
        // Compensate a proven partial operation before leaving its qualified execution point.
        // A failed compensation keeps its latest proven state for an explicit cleanup retry.
        if matches!(&result, Err(Error::PartialWrite { .. })) && l.expected.is_some()
            && self.memory.identity().is_ok_and(|id| id == self.id)
            && guard_matches(&self.memory, &s)
            && self.memory.can_write()
        {
            if let Some(previous) = previous { let _ = write_tracked(&mut self.memory, l, address, &previous); }
        }
        result
    }

    pub fn release(&mut self, owner: &str, handle: u64) -> Result<(), Error> {
        let l = self.leases.get(&handle).ok_or(Error::Refused)?;
        if l.owner != owner { return Err(Error::Refused); }
        let s = self.checked(&l.key)?.clone();
        let (address, _) = s.storage();
        let current = self.memory.read(address, l.original.len())?;
        if current == l.original && !l.notify { self.leases.remove(&handle); return Ok(()); }
        if current != l.original && Some(&current) != l.expected.as_ref() { return Err(Error::Conflict); }
        if !self.memory.can_write() { return Err(Error::Unavailable("not at the qualified execution point".into())); }
        let original = l.original.clone();
        let l = self.leases.get_mut(&handle).unwrap();
        if current != original {
            write_tracked(&mut self.memory, l, address, &original)?;
            l.notify = s.native.is_some() && value_of(&s, &current)? != value_of(&s, &original)?;
        }
        // Restore with the same semantics as the write: a restored setter value needs its
        // change handler. A failed call keeps the lease so a later cleanup retries only the call.
        if let (true, Some(native)) = (l.notify, &s.native) {
            self.memory.call(native.function, native.object)?;
        }
        self.leases.remove(&handle);
        Ok(())
    }

    fn native_bounds(&self, native: &Native, symbol: &Symbol, value: Scalar) -> Result<(), Error> {
        let Some((min, max)) = native.clamp else { return Ok(()) };
        let bound = |at| self.memory.read(at, symbol.kind.size()).and_then(|b| symbol.kind.decode(&b)).map(Scalar::number);
        let n = value.number();
        if n < bound(min)? || n > bound(max)? { return Err(Error::Invalid("value outside the native clamp".into())); }
        Ok(())
    }

    /// The read-only watch of v2 symbols (owner decision 2026-10-23: checks, then watch, then writes).
    /// Call it about once a second with a monotonic `now_ms`. Each call reads every unproved
    /// symbol: guards, value, mirror and clamp as at resolution. A failed read starts its watch
    /// again; `WATCH_READS` passes over `WATCH_MS` prove it. Returns the keys proved now.
    pub fn watch(&mut self, now_ms: u64) -> Vec<String> {
        let mut proved = vec![];
        let keys: Vec<String> = self.symbols.iter().filter(|(_, s)| !s.proved).map(|(k, _)| k.clone()).collect();
        for key in keys {
            let result = self.consistent(&key);
            let ok = result.is_ok();
            let s = self.symbols.get_mut(&key).unwrap();
            s.failed = result.err().map(|e| e.to_string());
            s.watch = match (ok, s.watch) {
                (false, _) => None,
                (true, None) => Some((now_ms, 1)),
                (true, Some((first, reads))) => Some((first, reads.saturating_add(1))),
            };
            if let Some((first, reads)) = s.watch {
                if reads >= WATCH_READS && now_ms.saturating_sub(first) >= WATCH_MS { s.proved = true; proved.push(key); }
            }
        }
        proved
    }

    /// True when `key` may take a writer now (always for v1 and read symbols).
    pub fn proved(&self, key: &str) -> bool { self.symbols.get(key).is_some_and(|s| s.proved) }

    /// The symbols of map `map_id` in this runtime, by symbol name.
    fn of_map<'a>(&'a self, map_id: &'a str) -> impl Iterator<Item = (&'a str, &'a Resolved)> + 'a {
        self.symbols.iter().filter_map(move |(k, s)| k.strip_prefix(map_id).and_then(|r| r.strip_prefix('/')).map(|name| (name, s)))
    }

    /// True while a symbol of map `map_id` waits for its watch.
    pub fn unproved(&self, map_id: &str) -> bool { self.of_map(map_id).any(|(_, s)| !s.proved) }

    /// Why the last watch read of an unproved symbol of map `map_id` failed, if it did: a check
    /// that fails each time keeps its writers on Busy, and the status must say why.
    pub fn watch_error(&self, map_id: &str) -> Option<String> {
        self.of_map(map_id).find_map(|(name, s)| s.failed.as_ref().filter(|_| !s.proved).map(|e| format!("{name}: {e}")))
    }

    /// The saved result of v2 map `map_id` once all its symbols are proved; None for an exact
    /// map, an unknown map, one that still waits for its watch, or an exe with no digest.
    pub fn proof(&self, map_id: &str) -> Option<Proof> {
        if !exe_digest(&self.id.sha256) { return None; }
        let (version, digest) = self.maps.get(map_id)?;
        let symbols: Option<BTreeMap<String, Found>> = self.of_map(map_id).map(|(name, s)| s.found.filter(|_| s.proved).map(|f| (name.to_string(), f))).collect();
        let symbols = symbols.filter(|s| !s.is_empty())?;
        Some(Proof { exe: self.id.sha256.clone(), map: map_id.into(), version: version.clone(), file: hex(digest), symbols })
    }

    fn consistent(&self, key: &str) -> Result<(), Error> {
        let s = self.checked(key)?;
        let Some(n) = &s.native else { return self.read(key).map(|_| ()) };
        let span = self.memory.read(n.span, n.len)?;
        setter_consistent(&s.symbol, n, &span, |a| self.memory.read(a, s.symbol.kind.size()).and_then(|b| s.symbol.kind.decode(&b)).map(Scalar::number))
    }

    /// Idempotent stop closes admission before restore. Errors stay visible and keep their locks.
    pub fn stop(&mut self, owner: &str) -> Vec<Error> {
        self.stopped.insert(owner.into());
        let ids: Vec<_> = self.leases.iter().filter(|(_, l)| l.owner == owner).map(|(h, _)| *h).collect();
        ids.into_iter().filter_map(|h| self.release(owner, h).err()).collect()
    }

    pub fn disconnect(&mut self) -> Vec<Error> {
        self.disconnected = true;
        let owners: BTreeSet<_> = self.leases.values().map(|l| l.owner.clone()).collect();
        owners.into_iter().flat_map(|o| self.stop(&o)).collect()
    }
}

