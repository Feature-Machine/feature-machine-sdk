// The control engine: raw device events in, control events and learned bindings out. Pure and
// time-driven (each call gets `now` in ms), so every rule has a unit test without devices: chords
// (the most specific binding wins), the baseline (a joystick's state after connect, from the
// device layer), the twin debounce (a same-path remove and add within DEBOUNCE_MS keeps held
// controls), the focus gate, and the release rule (gate close, device loss, binding change).
use crate::binding::{Binding, ControlDecl, Kind, Store};
use crate::device::{axis_value, DeviceRef, Fingerprint, Source, Tier};
use crate::element::{Element, HatDir, MoveAxis, Wheel};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The device layer's id of a live device (SDL instance id, raw input handle, evdev node).
pub type DevId = u64;

/// A device that goes away and comes back with the same path within this time is the same
/// device: SDL removes and adds a twin when its sibling connects (plan case 0.7c).
pub const DEBOUNCE_MS: u64 = 1000;
const AXIS_DOWN: f32 = 0.5;
const AXIS_UP: f32 = 0.35;
const LEARN_MOVE: i32 = 50;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Raw {
    Button { index: u16, down: bool },
    /// SDL value, -32768..32767 (before the descriptor fix).
    Axis { index: u8, value: i16 },
    /// SDL hat bits: up 1, right 2, down 4, left 8.
    Hat { index: u8, mask: u8 },
    Key { usage: u16, down: bool },
    MouseButton { n: u8, down: bool },
    /// In 1/120 notches (WHEEL_DELTA), positive up and right.
    Wheel { dx: i32, dy: i32 },
    Move { dx: i32, dy: i32 },
}

/// Wire form: `"kind":"button","value":true` (the same names as the control kinds).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Value {
    Button(bool),
    Axis(f32),
    Delta(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Control { feature: String, control: String, value: Value },
    /// `id`: the id of the learn that found it, so the app refuses a result that arrives after
    /// it cancelled that learn and started another one.
    Learned { binding: Binding, id: u64 },
}

/// One version of a feature. Controls and the focus gate use it, because a game runs the version
/// that it started with: after an update, two games can run two versions of one feature, each
/// with its own control list. Bindings stay by feature id, so they apply to all versions.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct FeatureRef {
    pub id: String,
    pub version: String,
}

impl FeatureRef {
    pub fn new(id: &str, version: &str) -> FeatureRef { FeatureRef { id: id.into(), version: version.into() } }
}

struct Dev {
    source: Source,
    fp: Option<Fingerprint>,
    wrapped: Vec<bool>,
    /// The state is real: keyboards and mice at once, a joystick at its baseline, a twin that took
    /// over a gone device's state at once.
    known: bool,
    /// Reports count. A joystick's reports before its baseline are dropped: SDL's first values after
    /// an open are not real (case 0.7d), and the baseline snapshot replaces them.
    ready: bool,
    gone_at: Option<u64>,
    /// Current value of each element: 0 or 1 for switches, -1..1 for axes.
    state: HashMap<Element, f32>,
    /// Axis values at the baseline: a trigger rests at -1, some axes rest off center.
    rest: HashMap<u8, f32>,
    /// Switches held at the baseline (a latched toggle, a stuck button): they count only after
    /// their first release, so a held switch never fires and never blocks later presses.
    unarmed: HashSet<Element>,
    axis_on: HashMap<u8, bool>,
    wheel: (i32, i32),
}

impl Dev {
    fn new(source: Source, fp: Option<Fingerprint>, wrapped: Vec<bool>) -> Dev {
        let ready = source != Source::Joystick; // keyboards and mice have no rest state
        Dev { source, fp, wrapped, known: ready, ready, gone_at: None, state: HashMap::new(), rest: HashMap::new(), unarmed: HashSet::new(), axis_on: HashMap::new(), wheel: (0, 0) }
    }
    fn value(&self, e: Element) -> f32 { self.state.get(&e).copied().unwrap_or(0.0) }
    fn rest(&self, axis: u8) -> f32 { self.rest.get(&axis).copied().unwrap_or(0.0) }
    /// Pressed now: an axis away from its rest, or an armed switch that is down.
    fn pressed(&self, e: Element) -> bool {
        match e.canonical() {
            Element::Axis(i) => self.axis_on.get(&i).copied().unwrap_or(false),
            c => self.state.iter().any(|(k, v)| k.canonical() == c && *v >= 0.5 && !self.unarmed.contains(k)),
        }
    }
    /// Stores a value and tells if it changed. A switch that goes up is armed.
    fn store(&mut self, e: Element, v: f32) -> bool {
        if v < 0.5 { self.unarmed.remove(&e); }
        if let Element::Axis(i) = e {
            let off = (v - self.rest(i)).abs();
            let on = self.axis_on.get(&i).copied().unwrap_or(false);
            self.axis_on.insert(i, if on { off > AXIS_UP } else { off > AXIS_DOWN });
        }
        self.state.insert(e, v) != Some(v)
    }
}

/// The element values in one report (not wheel or mouse movement).
fn elements(raw: Raw, wrapped: &[bool]) -> Vec<(Element, f32)> {
    let on = |b: bool| b as u8 as f32;
    match raw {
        Raw::Button { index, down } => vec![(Element::Button(index), on(down))],
        Raw::Key { usage, down } => vec![(Element::Key(usage), on(down))],
        Raw::MouseButton { n, down } => vec![(Element::MouseButton(n), on(down))],
        Raw::Hat { index, mask } => [(1, HatDir::Up), (2, HatDir::Right), (4, HatDir::Down), (8, HatDir::Left)]
            .into_iter().map(|(bit, dir)| (Element::Hat(index, dir), on(mask & bit != 0))).collect(),
        Raw::Axis { index, value } => vec![(Element::Axis(index), axis_value(value, wrapped.get(index as usize).copied().unwrap_or(false)))],
        Raw::Wheel { .. } | Raw::Move { .. } => vec![],
    }
}

/// The controls of `chosen` hits, each once, in order.
fn controls(hits: &[(usize, usize)]) -> Vec<usize> {
    let mut list: Vec<usize> = hits.iter().map(|h| h.0).collect();
    list.dedup();
    list
}

struct Ctl {
    owner: FeatureRef,
    id: String,
    kind: Kind,
    bindings: Vec<Binding>,
    /// Button: the devices that hold each binding down ("any keyboard" can be two keyboards).
    down: Vec<BTreeSet<DevId>>,
    axis: f32,
    /// Axis: the binding whose axis moved last; it drives the control among equal bindings.
    last: Option<usize>,
}

impl Ctl {
    fn active(&self) -> bool { self.down.iter().any(|s| !s.is_empty()) }
    fn rest(&self) -> Value {
        match self.kind {
            Kind::Button => Value::Button(false),
            Kind::Axis => Value::Axis(0.0),
            Kind::Delta => Value::Delta(0.0),
        }
    }
}

struct Learn {
    kind: Kind,
    id: u64,
    moved: HashMap<DevId, (i32, i32)>,
    /// A held switch that can be a chord modifier (a modifier key, a joystick switch): it is the
    /// trigger if it is released alone, else a modifier of the next press.
    pending: Option<(DevId, Element)>,
}

#[derive(Default)]
pub struct Engine {
    devs: BTreeMap<DevId, Dev>,
    ctls: Vec<Ctl>,
    /// The device paths (lower case) that the joystick bindings name.
    claimed: HashSet<String>,
    /// Each joystick that the store names (all features, also turned off): learn names a known
    /// controller as the store does, so a port move does not give it a second identity.
    saved: Vec<Fingerprint>,
    gate: bool,
    /// The feature versions that run in the game session in front: only their controls get
    /// events, and only their chords compete (a feature of another game must not hide a shorter
    /// chord, and another version must not send its controls to this game).
    live: BTreeSet<FeatureRef>,
    learn: Option<Learn>,
}

impl Engine {
    pub fn new() -> Self { Self::default() }

    /// The controls of the feature versions with their bindings from the store. A control with
    /// the same version, id, kind and bindings keeps its state (a session that starts elsewhere
    /// must not drop a held button); the others release first (release rule). Then axis controls
    /// get their value.
    pub fn set_controls(&mut self, features: &[(FeatureRef, Vec<ControlDecl>)], store: &Store) -> Vec<Out> {
        let mut old = std::mem::take(&mut self.ctls);
        let mut new: Vec<Ctl> = features.iter().flat_map(|(f, decls)| decls.iter().map(move |d| (f, d))).map(|(f, d)| {
            let bindings = store.bindings(&f.id, d);
            Ctl { owner: f.clone(), id: d.id.clone(), kind: d.kind, down: vec![BTreeSet::new(); bindings.len()], bindings, axis: 0.0, last: None }
        }).collect();
        for n in &mut new {
            let same = old.iter().position(|o| o.owner == n.owner && o.id == n.id && o.kind == n.kind && o.bindings == n.bindings);
            if let Some(o) = same.map(|i| old.swap_remove(i)) { (n.down, n.axis, n.last) = (o.down, o.axis, o.last); }
        }
        self.ctls = old;
        let mut out = self.release_all();
        self.ctls = new;
        self.saved = store.features.values().flat_map(|m| m.values()).flatten().filter_map(|b| match &b.device {
            DeviceRef::Joystick(fp) => Some(fp.clone()),
            DeviceRef::Any(_) => None,
        }).collect();
        self.saved.dedup();
        self.claimed = self.ctls.iter().flat_map(|c| &c.bindings).filter_map(|b| match &b.device {
            DeviceRef::Joystick(fp) => fp.path.as_deref().filter(|p| !p.is_empty()).map(str::to_ascii_lowercase),
            DeviceRef::Any(_) => None,
        }).collect();
        // A kept hold stays on a device that its binding can still mean: a path that a new binding
        // claims (a twin) is no longer a model match for the others (review R2-B1). Not "the device
        // that resolve picks now": in a twin churn a live twin outranks the held one in its debounce.
        for ci in 0..self.ctls.len() {
            for bi in 0..self.ctls[ci].bindings.len() {
                let lost: Vec<DevId> = self.ctls[ci].down[bi].iter().copied().filter(|d| !self.can_mean(&self.ctls[ci].bindings[bi], *d)).collect();
                for d in lost { self.press(ci, bi, d, false, &mut out); }
            }
        }
        self.sync_axes(&mut out);
        out
    }

    /// The focus gate: Some (the feature versions that run in the session in front) opens it,
    /// None closes it. A new set releases what was held and gives the axes their values again.
    pub fn set_gate(&mut self, live: Option<BTreeSet<FeatureRef>>) -> Vec<Out> {
        let open = live.is_some();
        let live = live.unwrap_or_default();
        if open == self.gate && live == self.live { return vec![]; }
        let mut out = if self.gate { self.release_all() } else { vec![] };
        self.gate = open;
        self.live = live;
        if open { self.sync_axes(&mut out); }
        out
    }

    /// New starts of these features in the game in front, with the same version (session sync
    /// plan 12.2, control gate): the new start never got the axis values, so their axis controls
    /// give their value again with no device move. Other features keep their holds.
    pub fn restarted(&mut self, ids: &BTreeSet<String>) -> Vec<Out> {
        if !self.gate || self.learn.is_some() { return vec![]; }
        self.ctls.iter()
            .filter(|c| c.kind == Kind::Axis && ids.contains(&c.owner.id) && self.live.contains(&c.owner))
            .map(|c| Out::Control { feature: c.owner.id.clone(), control: c.id.clone(), value: Value::Axis(c.axis) })
            .collect()
    }

    fn is_live(&self, c: &Ctl) -> bool { self.live.contains(&c.owner) }

    pub fn added(&mut self, dev: DevId, source: Source, fp: Option<Fingerprint>, wrapped: Vec<bool>) {
        // Twin churn or a quick replug: take over the state of the gone device with this path.
        let back = fp.as_ref().and_then(|fp| {
            self.devs.iter().find(|(_, d)| d.gone_at.is_some() && d.fp.as_ref().and_then(|g| g.matches(fp)) >= Some(Tier::Path)).map(|(id, _)| *id)
        });
        let mut d = Dev::new(source, fp, wrapped);
        if let Some((old_id, old)) = back.and_then(|id| self.devs.remove(&id).map(|o| (id, o))) {
            // Its state stays known (axis controls keep their value); its reports wait for the baseline.
            (d.known, d.state, d.rest, d.unarmed, d.axis_on) = (old.known, old.state, old.rest, old.unarmed, old.axis_on);
            for set in self.ctls.iter_mut().flat_map(|c| &mut c.down) {
                if set.remove(&old_id) { set.insert(dev); }
            }
            if let Some(p) = self.learn.as_mut().and_then(|l| l.pending.as_mut()).filter(|p| p.0 == old_id) { p.0 = dev; }
        }
        self.devs.insert(dev, d);
    }

    /// The joystick's state after connect, which the device layer reads when SDL has real reports
    /// (case 0.7d: the values at the add are not real, and SDL sends no event for an axis that
    /// does not move). Axes get their rest; held switches wait for a release. For a twin that
    /// took over a known state, the snapshot is ordinary input: only real changes fire.
    pub fn baseline(&mut self, dev: DevId, snapshot: &[Raw]) -> Vec<Out> {
        let mut out = vec![];
        let Some(d) = self.devs.get_mut(&dev) else { return out };
        d.ready = true;
        if d.known {
            let values: Vec<(Element, f32)> = snapshot.iter().flat_map(|r| elements(*r, &d.wrapped)).collect();
            self.apply(dev, values, &mut out);
            return out;
        }
        let values: Vec<(Element, f32)> = snapshot.iter().flat_map(|r| elements(*r, &d.wrapped)).collect();
        for (e, v) in values {
            d.state.insert(e, v);
            match e {
                Element::Axis(i) => {
                    d.rest.insert(i, v);
                    d.axis_on.insert(i, false);
                }
                _ if v >= 0.5 => { d.unarmed.insert(e); }
                _ => {}
            }
        }
        d.known = true;
        self.sync_axes(&mut out);
        out
    }

    pub fn removed(&mut self, dev: DevId, now: u64) {
        if let Some(d) = self.devs.get_mut(&dev) {
            d.gone_at = Some(now);
        }
    }

    /// Ends the debounce of gone devices: their held controls go up, their axes to rest.
    pub fn tick(&mut self, now: u64) -> Vec<Out> {
        let gone: Vec<DevId> = self.devs.iter().filter(|(_, d)| d.gone_at.is_some_and(|t| now >= t + DEBOUNCE_MS)).map(|(id, _)| *id).collect();
        let mut out = vec![];
        for id in gone {
            self.devs.remove(&id);
            if let Some(l) = self.learn.as_mut().filter(|l| l.pending.is_some_and(|p| p.0 == id)) { l.pending = None; }
            for c in &mut self.ctls {
                let was = c.active();
                c.down.iter_mut().for_each(|s| { s.remove(&id); });
                if was && !c.active() && self.gate {
                    out.push(Out::Control { feature: c.owner.id.clone(), control: c.id.clone(), value: Value::Button(false) });
                }
            }
        }
        self.sync_axes(&mut out);
        out
    }

    /// Starts learn mode for one control kind; control events stop until a binding is learned
    /// or `cancel_learn`. Learn ignores the focus gate: the member is in the app.
    pub fn learn(&mut self, kind: Kind, id: u64) -> Vec<Out> {
        let out = self.release_all();
        self.learn = Some(Learn { kind, id, moved: HashMap::new(), pending: None });
        out
    }

    pub fn cancel_learn(&mut self) { self.learn = None; }

    pub fn input(&mut self, dev: DevId, raw: Raw) -> Vec<Out> {
        let mut out = vec![];
        let Some(d) = self.devs.get_mut(&dev) else { return out };
        if d.gone_at.is_some() || !d.ready { return out; }
        match raw {
            Raw::Wheel { dx, dy } => self.wheel(dev, dx, dy, &mut out),
            Raw::Move { dx, dy } => self.mouse_move(dev, dx, dy, &mut out),
            _ => {
                let values = elements(raw, &d.wrapped);
                self.apply(dev, values, &mut out);
            }
        }
        out
    }

    /// Stores all values of one report or snapshot first, then handles the changes, releases
    /// before presses: a chord then sees the new state of all its modifiers (a snapshot can
    /// list a trigger before a modifier that went up).
    fn apply(&mut self, dev: DevId, values: Vec<(Element, f32)>, out: &mut Vec<Out>) {
        let d = self.devs.get_mut(&dev).expect("known device");
        let mut changed: Vec<(bool, Element)> = values.into_iter().filter(|(e, v)| d.store(*e, *v)).map(|(e, v)| (v >= 0.5, e)).collect();
        changed.sort_by_key(|(down, _)| *down);
        for (_, e) in changed { self.element(dev, e, out); }
    }

    fn element(&mut self, dev: DevId, e: Element, out: &mut Vec<Out>) {
        // Switch transitions, on the canonical element (left and right Ctrl are one key).
        let c = e.canonical();
        let pressed = self.devs[&dev].pressed(c);
        if self.learn.is_some() {
            // The left mouse button clicks in the app (Cancel, other pages) while learn waits,
            // so learn never takes it: a click on Cancel would else bind that click.
            if c == Element::MouseButton(1) { return; }
            let joystick = self.devs[&dev].source == Source::Joystick;
            let l = self.learn.as_mut().unwrap();
            let axis = matches!(c, Element::Axis(_));
            let mut learn_now = None;
            match l.kind {
                Kind::Axis => if axis && pressed { learn_now = Some(c) },
                Kind::Delta => {}
                Kind::Button if axis => if pressed && joystick { learn_now = Some(c) },
                // Ctrl then R learns Ctrl+R; a HOTAS shift button then the hat learns shift+hat.
                Kind::Button if c.is_modifier() || joystick => {
                    if pressed {
                        if l.pending.is_none() { l.pending = Some((dev, c)); } else if joystick { learn_now = Some(c); }
                    } else if l.pending == Some((dev, c)) {
                        learn_now = Some(c);
                    }
                }
                Kind::Button => if pressed { learn_now = Some(c) },
            }
            if let Some(t) = learn_now { self.learned(dev, t, out); }
            return;
        }
        if !self.gate { return; }
        if let Element::Axis(i) = e {
            let moved: Vec<(usize, usize)> = self.ctls.iter().enumerate().filter(|(_, c)| c.kind == Kind::Axis)
                .filter_map(|(ci, c)| c.bindings.iter().position(|b| b.trigger == Element::Axis(i) && self.bound_to(b, dev)).map(|bi| (ci, bi))).collect();
            for (ci, bi) in moved { self.ctls[ci].last = Some(bi); }
        }
        if pressed {
            for (ci, bi) in self.chosen(dev, &[Kind::Button], |t| t.canonical() == c) {
                self.press(ci, bi, dev, true, out);
            }
        } else {
            for ci in 0..self.ctls.len() {
                for bi in 0..self.ctls[ci].bindings.len() {
                    if self.ctls[ci].down[bi].contains(&dev) && self.ctls[ci].bindings[bi].trigger.canonical() == c {
                        self.press(ci, bi, dev, false, out);
                    }
                }
            }
        }
        // An axis move, or a modifier that selects another axis binding.
        self.sync_axes(out);
    }

    fn press(&mut self, ci: usize, bi: usize, dev: DevId, down: bool, out: &mut Vec<Out>) {
        let c = &mut self.ctls[ci];
        let was = c.active();
        if down { c.down[bi].insert(dev); } else { c.down[bi].remove(&dev); }
        if was != c.active() {
            out.push(Out::Control { feature: c.owner.id.clone(), control: c.id.clone(), value: Value::Button(c.active()) });
        }
    }

    fn set_axis(&mut self, ci: usize, v: f32, out: &mut Vec<Out>) {
        let c = &mut self.ctls[ci];
        if (c.axis - v).abs() > 1e-3 || (v == 0.0) != (c.axis == 0.0) {
            c.axis = v;
            out.push(Out::Control { feature: c.owner.id.clone(), control: c.id.clone(), value: Value::Axis(v) });
        }
    }

    fn wheel(&mut self, dev: DevId, dx: i32, dy: i32, out: &mut Vec<Out>) {
        if let Some(l) = &self.learn {
            let kind = l.kind;
            let (axis, step) = if dy != 0 {
                (MoveAxis::Y, if dy > 0 { Wheel::Up } else { Wheel::Down })
            } else {
                (MoveAxis::X, if dx > 0 { Wheel::Right } else { Wheel::Left })
            };
            match kind {
                Kind::Button => self.learned(dev, Element::WheelStep(step), out),
                Kind::Delta => self.learned(dev, Element::WheelAxis(axis), out),
                Kind::Axis => {}
            }
            return;
        }
        if !self.gate { return; }
        let d = self.devs.get_mut(&dev).unwrap();
        d.wheel.0 += dx;
        d.wheel.1 += dy;
        let mut steps = vec![];
        for (acc, pos, neg) in [(&mut d.wheel.1, Wheel::Up, Wheel::Down), (&mut d.wheel.0, Wheel::Right, Wheel::Left)] {
            while *acc >= 120 { *acc -= 120; steps.push(pos); }
            while *acc <= -120 { *acc += 120; steps.push(neg); }
        }
        for (moved, axis, dir) in [(dy, MoveAxis::Y, if dy > 0 { Wheel::Up } else { Wheel::Down }), (dx, MoveAxis::X, if dx > 0 { Wheel::Right } else { Wheel::Left })] {
            if moved == 0 { continue; }
            let hits = self.chosen(dev, &[Kind::Delta, Kind::Button], |t| t == Element::WheelAxis(axis) || t == Element::WheelStep(dir));
            let notches = steps.iter().filter(|s| **s == dir).count();
            for ci in controls(&hits) {
                let ctl = &self.ctls[ci];
                let event = |value| Out::Control { feature: ctl.owner.id.clone(), control: ctl.id.clone(), value };
                if ctl.kind == Kind::Delta {
                    out.push(event(Value::Delta(moved as f32 / 120.0)));
                } else if !ctl.active() {
                    // A notch is a press and a release, but not of a control that another binding holds.
                    for _ in 0..notches { out.extend([event(Value::Button(true)), event(Value::Button(false))]); }
                }
            }
        }
    }

    fn mouse_move(&mut self, dev: DevId, dx: i32, dy: i32, out: &mut Vec<Out>) {
        if let Some(l) = &mut self.learn {
            if l.kind == Kind::Delta {
                let m = l.moved.entry(dev).or_default();
                m.0 += dx.abs();
                m.1 += dy.abs();
                let m = *m;
                if m.0.max(m.1) >= LEARN_MOVE {
                    self.learned(dev, Element::Move(if m.0 >= m.1 { MoveAxis::X } else { MoveAxis::Y }), out);
                }
            }
            return;
        }
        if !self.gate { return; }
        for (n, axis) in [(dx, MoveAxis::X), (dy, MoveAxis::Y)] {
            if n == 0 { continue; }
            let hits = self.chosen(dev, &[Kind::Delta], |t| t == Element::Move(axis));
            for ci in controls(&hits) {
                let (ctl, bi) = (&self.ctls[ci], hits.iter().find(|h| h.0 == ci).unwrap().1);
                let v = if ctl.bindings[bi].tuning.invert { -n } else { n } as f32;
                out.push(Out::Control { feature: ctl.owner.id.clone(), control: ctl.id.clone(), value: Value::Delta(v) });
            }
        }
    }

    /// The bindings that one input fires: of these control kinds, on this device, with a trigger
    /// that `on` accepts and the chord held; of those only the ones with the most held modifiers
    /// (the most specific chord wins, so Ctrl+wheel does not also fire a plain wheel binding).
    fn chosen(&self, dev: DevId, kinds: &[Kind], on: impl Fn(Element) -> bool) -> Vec<(usize, usize)> {
        let mut hits = vec![];
        for (ci, ctl) in self.ctls.iter().enumerate().filter(|(_, c)| kinds.contains(&c.kind) && self.is_live(c)) {
            for (bi, b) in ctl.bindings.iter().enumerate() {
                if on(b.trigger) && self.bound_to(b, dev) && self.mods_held(b, dev) { hits.push((ci, bi, b.modifiers.len())); }
            }
        }
        let best = hits.iter().map(|h| h.2).max().unwrap_or(0);
        hits.into_iter().filter(|h| h.2 == best).map(|(ci, bi, _)| (ci, bi)).collect()
    }

    /// The learned binding: device, trigger, and the switches held with it on the same device
    /// (or any keyboard, for keyboard and mouse triggers) as modifiers. An axis that rests at an
    /// end becomes 0..1.
    fn learned(&mut self, dev: DevId, trigger: Element, out: &mut Vec<Out>) {
        let Some((kind, id)) = self.learn.as_ref().map(|l| (l.kind, l.id)) else { return };
        let d = &self.devs[&dev];
        // A controller that saved bindings already reach (moved to another port: path P1 saved,
        // live at P3) keeps that saved name. A new P3 name would claim P3, and then the old P1
        // bindings could not use the model match (resolve) any more.
        let device = match (&d.fp, d.source) {
            (Some(fp), Source::Joystick) => DeviceRef::Joystick(self.saved.iter().find(|s| self.resolve(s) == Some(dev)).unwrap_or(fp).clone()),
            (_, s) => DeviceRef::Any(s),
        };
        let mut modifiers: Vec<Element> = if d.source == Source::Joystick {
            d.state.keys().filter(|e| e.is_switch() && !matches!(e, Element::Axis(_)) && **e != trigger && d.pressed(**e)).copied().collect()
        } else {
            self.devs.values().filter(|o| o.source == Source::Keyboard && o.gone_at.is_none())
                .flat_map(|o| o.state.keys().filter(|e| e.is_modifier() && o.pressed(**e)).map(|e| e.canonical()))
                .filter(|e| *e != trigger).collect()
        };
        modifiers.sort();
        modifiers.dedup();
        let mut b = Binding { device, trigger, modifiers, tuning: Default::default() };
        if let Element::Axis(i) = trigger {
            b.tuning.half = d.rest(i).abs() > 0.9;
            b.tuning.invert = d.rest(i) > 0.9;
        }
        if b.fits(kind) {
            self.learn = None;
            out.push(Out::Learned { binding: b, id });
        }
    }

    fn bound_to(&self, b: &Binding, dev: DevId) -> bool {
        match &b.device {
            DeviceRef::Any(s) => self.devs.get(&dev).is_some_and(|d| d.source == *s),
            DeviceRef::Joystick(fp) => self.resolve(fp) == Some(dev),
        }
    }

    /// The device that a saved fingerprint means: the strongest match, a live device before one in
    /// its debounce (which keeps its bindings until then), then the lowest id. A match weaker than
    /// the path never takes a device whose path another binding names: that is a twin with its
    /// own bindings, so an unplugged twin does not give its controls to the other.
    fn resolve(&self, fp: &Fingerprint) -> Option<DevId> {
        self.devs.iter().filter_map(|(id, d)| Some((self.tier(fp, d)?, d.gone_at.is_none(), std::cmp::Reverse(*id)))).max().map(|(_, _, r)| r.0)
    }

    /// How well a saved fingerprint matches a device, if the device can be that one at all.
    fn tier(&self, fp: &Fingerprint, d: &Dev) -> Option<Tier> {
        let live = d.fp.as_ref()?;
        let tier = fp.matches(live)?;
        let claimed = live.path.as_ref().is_some_and(|p| self.claimed.contains(&p.to_ascii_lowercase()));
        (tier >= Tier::Path || !claimed).then_some(tier)
    }

    /// The binding can mean this device (also one in its debounce, or one that another device
    /// outranks): the rule for a held device to stay held when the controls change.
    fn can_mean(&self, b: &Binding, dev: DevId) -> bool {
        match &b.device {
            DeviceRef::Any(_) => self.bound_to(b, dev),
            DeviceRef::Joystick(fp) => self.devs.get(&dev).is_some_and(|d| self.tier(fp, d).is_some()),
        }
    }

    /// Keys count on any keyboard and mouse buttons on any mouse (Alt+wheel, Ctrl+mouse button);
    /// joystick switches count on the trigger's device (a HOTAS shift button).
    fn mods_held(&self, b: &Binding, dev: DevId) -> bool {
        b.modifiers.iter().all(|m| {
            let on = |s: Source| self.devs.values().any(|d| d.source == s && d.gone_at.is_none() && d.pressed(*m));
            match m {
                Element::Key(_) => on(Source::Keyboard),
                Element::MouseButton(_) => on(Source::Mouse),
                _ => self.devs.get(&dev).is_some_and(|d| d.pressed(*m)),
            }
        })
    }

    fn release_all(&mut self) -> Vec<Out> {
        let gate = self.gate;
        let mut out = vec![];
        for c in &mut self.ctls {
            let held = c.active() || (c.kind == Kind::Axis && c.axis != 0.0);
            c.down.iter_mut().for_each(BTreeSet::clear);
            if held && gate {
                c.axis = 0.0;
                out.push(Out::Control { feature: c.owner.id.clone(), control: c.id.clone(), value: c.rest() });
            }
        }
        out
    }

    /// Gives each axis control the value of its selected binding (`axis_pick`) after an axis or
    /// modifier change, a gate open, a binding change, a baseline or a device loss. A control with
    /// no ready bound device goes to rest; one whose chord is not held keeps its value.
    fn sync_axes(&mut self, out: &mut Vec<Out>) {
        if !self.gate || self.learn.is_some() { return; }
        for ci in 0..self.ctls.len() {
            if self.ctls[ci].kind != Kind::Axis || !self.is_live(&self.ctls[ci]) { continue; }
            match self.axis_pick(ci) {
                Some(Some(v)) => self.set_axis(ci, v, out),
                Some(None) => {}
                None => self.set_axis(ci, 0.0, out),
            }
        }
    }

    /// The value of an axis control: from its binding with the most held modifiers (then the one
    /// that moved last), unless a binding of another axis control on the same axis holds more (the
    /// most specific chord wins, as for buttons). Some(None): a bound device has a known state but
    /// no chord is held. None: no bound device has a known state.
    fn axis_pick(&self, ci: usize) -> Option<Option<f32>> {
        let ctl = &self.ctls[ci];
        let mut found = false;
        let mut best: Option<((usize, bool), f32)> = None;
        for (bi, b) in ctl.bindings.iter().enumerate() {
            let (Element::Axis(i), DeviceRef::Joystick(fp)) = (b.trigger, &b.device) else { continue };
            let Some(dev) = self.resolve(fp) else { continue };
            let d = &self.devs[&dev];
            if !d.known || !d.state.contains_key(&b.trigger) { continue; }
            found = true;
            let n = b.modifiers.len();
            if !self.mods_held(b, dev) || self.axis_shadowed(dev, i, n) { continue; }
            // Among equal chords, the binding that moved last (two levers on one control).
            let rank = (n, ctl.last == Some(bi));
            if best.is_none_or(|(r, _)| rank > r) { best = Some((rank, b.tuning.apply(d.value(b.trigger)))); }
        }
        found.then_some(best.map(|(_, v)| v))
    }

    fn axis_shadowed(&self, dev: DevId, axis: u8, n: usize) -> bool {
        self.ctls.iter().filter(|c| c.kind == Kind::Axis && self.is_live(c)).flat_map(|c| &c.bindings)
            .any(|b| b.trigger == Element::Axis(axis) && b.modifiers.len() > n && self.bound_to(b, dev) && self.mods_held(b, dev))
    }
}

#[cfg(test)]
mod tests;
