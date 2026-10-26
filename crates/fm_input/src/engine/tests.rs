// Engine rules from the plan, the slice 0 results and review round 1, one test each, with no
// devices.
use super::*;
use crate::binding::{Binding, ControlDecl, Kind, Store, STORE_VERSION};
use crate::device::{DeviceRef, Fingerprint, Source};
use crate::element::Element;

const KBD: DevId = 1;
const MOUSE: DevId = 2;
const KBD2: DevId = 3;
const R: u16 = 0x15;
const CTRL: u16 = 0xE0;
const RCTRL: u16 = 0xE4;
const ALT: u16 = 0xE2;

fn decl(id: &str, kind: Kind, default: Option<&str>) -> ControlDecl {
    ControlDecl { id: id.into(), kind, label: id.into(), default: default.map(Into::into) }
}

fn stick(path: &str) -> Fingerprint {
    Fingerprint { vid: 0x1209, pid: 0x000A, name: "Twin".into(), guid: String::new(), serial: None, path: Some(path.into()), axes: 2, buttons: 8, hats: 1 }
}

fn joy(trigger: Element, path: &str) -> Binding {
    Binding { device: DeviceRef::Joystick(stick(path)), trigger, modifiers: vec![], tuning: Default::default() }
}

fn any(source: Source, trigger: Element) -> Binding {
    Binding { device: DeviceRef::Any(source), trigger, modifiers: vec![], tuning: Default::default() }
}

/// Version 1 of feature "f".
fn f() -> FeatureRef {
    FeatureRef::new("f", "1")
}

/// The gate open for feature "f".
fn live() -> Option<BTreeSet<FeatureRef>> {
    Some([f()].into())
}

/// An engine with keyboard and mouse, the gate open, and these controls of feature "f".
fn engine(decls: Vec<ControlDecl>, store: Store) -> Engine {
    let mut e = Engine::new();
    e.added(KBD, Source::Keyboard, None, vec![]);
    e.added(MOUSE, Source::Mouse, None, vec![]);
    e.set_controls(&[(f(), decls)], &store);
    e.set_gate(live());
    e
}

fn store(entries: Vec<(&str, Vec<Binding>)>) -> Store {
    let mut s = Store { version: STORE_VERSION, ..Default::default() };
    for (control, bindings) in entries {
        s.features.entry("f".into()).or_default().insert(control.into(), bindings);
    }
    s
}

/// A joystick connects, then the device layer sends its state (the baseline).
fn plug(e: &mut Engine, id: DevId, path: &str, snapshot: &[Raw]) -> Vec<Out> {
    e.added(id, Source::Joystick, Some(stick(path)), vec![]);
    e.baseline(id, snapshot)
}

fn key(e: &mut Engine, usage: u16, down: bool) -> Vec<Out> {
    e.input(KBD, Raw::Key { usage, down })
}

fn button(e: &mut Engine, dev: DevId, index: u16, down: bool) -> Vec<Out> {
    e.input(dev, Raw::Button { index, down })
}

fn ctl(id: &str, value: Value) -> Out {
    Out::Control { feature: "f".into(), control: id.into(), value }
}

#[test]
fn chord_fires_only_with_its_modifier_and_most_specific_wins() {
    let mut e = engine(vec![decl("plain", Kind::Button, Some("r")), decl("chord", Kind::Button, Some("ctrl+r"))], Store::default());
    assert_eq!(key(&mut e, R, true), vec![ctl("plain", Value::Button(true))]);
    assert_eq!(key(&mut e, R, false), vec![ctl("plain", Value::Button(false))]);
    assert!(key(&mut e, RCTRL, true).is_empty()); // right Ctrl counts as Ctrl
    assert_eq!(key(&mut e, R, true), vec![ctl("chord", Value::Button(true))]);
    assert!(key(&mut e, RCTRL, false).is_empty()); // held until the trigger goes up
    assert_eq!(key(&mut e, R, false), vec![ctl("chord", Value::Button(false))]);
}

#[test]
fn gate_closed_gives_nothing_and_closing_releases() {
    let mut e = engine(vec![decl("t", Kind::Button, Some("r"))], Store::default());
    assert_eq!(key(&mut e, R, true), vec![ctl("t", Value::Button(true))]);
    assert_eq!(e.set_gate(None), vec![ctl("t", Value::Button(false))]);
    assert!(key(&mut e, R, false).is_empty());
    assert!(key(&mut e, R, true).is_empty());
    assert!(e.set_gate(live()).is_empty()); // a key held from before does not fire
}

#[test]
fn baseline_gives_rest_and_held_switches_fire_after_one_release() {
    // Case 0.7d: a button held at rest and a trigger at -1 come right after the add.
    let mut half = joy(Element::Axis(2), "p1");
    half.tuning.half = true;
    let decls = vec![decl("b", Kind::Button, None), decl("a", Kind::Axis, None)];
    let mut e = engine(decls, store(vec![("b", vec![joy(Element::Button(0), "p1")]), ("a", vec![half])]));
    e.added(10, Source::Joystick, Some(stick("p1")), vec![]);
    // Before the baseline, values are only stored.
    assert!(button(&mut e, 10, 0, true).is_empty());
    assert!(e.input(10, Raw::Axis { index: 2, value: -32768 }).is_empty());
    assert!(e.baseline(10, &[Raw::Button { index: 0, down: true }, Raw::Axis { index: 2, value: -32768 }]).is_empty());
    // The held button is not a press, and its release is not one either; then it works (review 4).
    assert!(button(&mut e, 10, 0, false).is_empty());
    assert_eq!(button(&mut e, 10, 0, true), vec![ctl("b", Value::Button(true))]);
    assert_eq!(e.input(10, Raw::Axis { index: 2, value: 32767 }), vec![ctl("a", Value::Axis(1.0))]);
}

#[test]
fn twin_churn_keeps_held_controls_and_real_loss_releases() {
    let mut e = engine(vec![decl("b", Kind::Button, None)], store(vec![("b", vec![joy(Element::Button(1), "p1")])]));
    plug(&mut e, 10, "p1", &[]);
    plug(&mut e, 11, "p2", &[]);
    assert_eq!(button(&mut e, 10, 1, true), vec![ctl("b", Value::Button(true))]);
    // The twin p2 reconnects; SDL removes and adds p1 too (new instance id 12).
    e.removed(10, 2000);
    e.removed(11, 2000);
    e.added(12, Source::Joystick, Some(stick("p1")), vec![]);
    assert!(button(&mut e, 12, 1, true).is_empty());
    assert!(e.baseline(12, &[Raw::Button { index: 1, down: true }]).is_empty()); // known state: plain input
    assert!(e.tick(3500).is_empty());
    assert_eq!(button(&mut e, 12, 1, false), vec![ctl("b", Value::Button(false))]);
    // A real unplug releases after the debounce.
    assert_eq!(button(&mut e, 12, 1, true), vec![ctl("b", Value::Button(true))]);
    e.removed(12, 6000);
    assert!(e.tick(6500).is_empty());
    assert_eq!(e.tick(7000), vec![ctl("b", Value::Button(false))]);
}

#[test]
fn bindings_follow_the_path_not_the_instance() {
    let mut e = engine(vec![decl("b", Kind::Button, None)], store(vec![("b", vec![joy(Element::Button(1), "p2")])]));
    plug(&mut e, 10, "p1", &[]);
    plug(&mut e, 11, "p2", &[]);
    assert!(button(&mut e, 10, 1, true).is_empty());
    assert_eq!(button(&mut e, 11, 1, true), vec![ctl("b", Value::Button(true))]);
}

#[test]
fn an_unplugged_twin_does_not_give_its_bindings_to_the_other() {
    // Review 2: each twin has its own binding; p1 goes away for good.
    let decls = vec![decl("one", Kind::Button, None), decl("two", Kind::Button, None)];
    let mut e = engine(decls, store(vec![("one", vec![joy(Element::Button(1), "p1")]), ("two", vec![joy(Element::Button(1), "p2")])]));
    plug(&mut e, 10, "p1", &[]);
    plug(&mut e, 11, "p2", &[]);
    e.removed(10, 1000);
    e.tick(2000);
    assert_eq!(button(&mut e, 11, 1, true), vec![ctl("two", Value::Button(true))]);
    // A binding whose path no live device has still finds the same model (a move to another port).
    let mut e = engine(vec![decl("one", Kind::Button, None)], store(vec![("one", vec![joy(Element::Button(1), "p1")])]));
    plug(&mut e, 12, "p3", &[]);
    assert_eq!(button(&mut e, 12, 1, true), vec![ctl("one", Value::Button(true))]);
}

// Slice 4 review round 3: a binding learned after a port move names the controller as the store
// does (p1), so the old bindings keep their model match. A twin that has its own saved path
// still gets its own name.
#[test]
fn learn_after_a_port_move_keeps_the_saved_name() {
    let decls = vec![decl("one", Kind::Button, None), decl("two", Kind::Button, None)];
    let mut e = engine(decls.clone(), store(vec![("one", vec![joy(Element::Button(1), "p1")])]));
    plug(&mut e, 12, "p3", &[]);
    // A joystick button alone learns at its release (it can be a HOTAS shift button).
    e.learn(Kind::Button, 1);
    button(&mut e, 12, 2, true);
    let out = button(&mut e, 12, 2, false);
    let [Out::Learned { binding: b, .. }] = &out[..] else { panic!("{out:?}") };
    assert_eq!(b.device, DeviceRef::Joystick(stick("p1")));
    e.set_controls(&[(f(), decls.clone())], &store(vec![("one", vec![joy(Element::Button(1), "p1")]), ("two", vec![b.clone()])]));
    assert_eq!(button(&mut e, 12, 1, true), vec![ctl("one", Value::Button(true))], "the old binding still works");
    assert_eq!(button(&mut e, 12, 2, true), vec![ctl("two", Value::Button(true))]);

    let mut e = engine(decls, store(vec![("one", vec![joy(Element::Button(1), "p1")])]));
    plug(&mut e, 10, "p1", &[]);
    plug(&mut e, 11, "p2", &[]);
    e.learn(Kind::Button, 2);
    button(&mut e, 11, 2, true);
    let out = button(&mut e, 11, 2, false);
    assert!(matches!(&out[..], [Out::Learned { binding: b, .. }] if b.device == DeviceRef::Joystick(stick("p2"))), "{out:?}");
}

// Plan 12.2 control gate: A1 runs, its stop report is replaced, A2 (same version) runs. The
// axis does not move, so only `restarted` gives A2 its value; a held button of another feature
// stays held.
#[test]
fn a_new_start_gets_the_axis_value_again() {
    let mut e = engine(vec![decl("a", Kind::Axis, None)], store(vec![("a", vec![joy(Element::Axis(0), "p1")])]));
    plug(&mut e, 10, "p1", &[Raw::Axis { index: 0, value: 0 }]);
    assert_eq!(e.input(10, Raw::Axis { index: 0, value: 16384 }), vec![ctl("a", Value::Axis(16384.0 / 32767.0))]);
    assert!(e.set_gate(live()).is_empty(), "same versions: the gate sees no change");
    assert_eq!(e.restarted(&["f".to_string()].into()), vec![ctl("a", Value::Axis(16384.0 / 32767.0))]);
    assert!(e.restarted(&["other".to_string()].into()).is_empty(), "another feature: nothing");
    e.set_gate(None);
    assert!(e.restarted(&["f".to_string()].into()).is_empty(), "gate closed: nothing");
}

#[test]
fn an_axis_keeps_its_value_through_the_debounce() {
    // Review 7: twin churn must not send rest and then the old value again.
    let mut e = engine(vec![decl("a", Kind::Axis, None)], store(vec![("a", vec![joy(Element::Axis(0), "p1")])]));
    plug(&mut e, 10, "p1", &[Raw::Axis { index: 0, value: 0 }]);
    assert_eq!(e.input(10, Raw::Axis { index: 0, value: 16384 }), vec![ctl("a", Value::Axis(16384.0 / 32767.0))]);
    e.removed(10, 1000);
    assert!(e.tick(1100).is_empty());
    e.added(12, Source::Joystick, Some(stick("p1")), vec![]);
    assert!(e.tick(2500).is_empty());
    e.removed(12, 3000);
    assert_eq!(e.tick(4000), vec![ctl("a", Value::Axis(0.0))]);
}

#[test]
fn any_keyboard_holds_while_one_keyboard_holds() {
    // Review 8: R on two keyboards, released in both orders and by an unplug.
    let mut e = engine(vec![decl("t", Kind::Button, Some("r"))], Store::default());
    e.added(KBD2, Source::Keyboard, None, vec![]);
    assert_eq!(key(&mut e, R, true), vec![ctl("t", Value::Button(true))]);
    assert!(e.input(KBD2, Raw::Key { usage: R, down: true }).is_empty());
    assert!(e.input(KBD2, Raw::Key { usage: R, down: false }).is_empty());
    assert_eq!(key(&mut e, R, false), vec![ctl("t", Value::Button(false))]);
    assert_eq!(e.input(KBD2, Raw::Key { usage: R, down: true }), vec![ctl("t", Value::Button(true))]);
    assert!(key(&mut e, R, true).is_empty());
    e.removed(KBD2, 1000);
    assert!(e.tick(2000).is_empty());
    assert_eq!(key(&mut e, R, false), vec![ctl("t", Value::Button(false))]);
}

#[test]
fn a_wheel_notch_does_not_release_a_held_control() {
    // Review 9: R and wheel up drive the same control.
    let bindings = vec![any(Source::Keyboard, Element::Key(R)), any(Source::Mouse, Element::WheelStep(Wheel::Up))];
    let mut e = engine(vec![decl("t", Kind::Button, None)], store(vec![("t", bindings)]));
    assert_eq!(key(&mut e, R, true), vec![ctl("t", Value::Button(true))]);
    assert!(e.input(MOUSE, Raw::Wheel { dx: 0, dy: 120 }).is_empty());
    assert_eq!(key(&mut e, R, false), vec![ctl("t", Value::Button(false))]);
    assert_eq!(e.input(MOUSE, Raw::Wheel { dx: 0, dy: 120 }), vec![ctl("t", Value::Button(true)), ctl("t", Value::Button(false))]);
}

#[test]
fn keyboard_modifiers_work_with_mouse_triggers() {
    // Review 5: a default Alt+mouse button 4, and a learned Ctrl+wheel up.
    let mut e = engine(vec![decl("m", Kind::Button, Some("alt+mouse:button:4"))], Store::default());
    assert!(e.input(MOUSE, Raw::MouseButton { n: 4, down: true }).is_empty());
    e.input(MOUSE, Raw::MouseButton { n: 4, down: false });
    key(&mut e, ALT, true);
    assert_eq!(e.input(MOUSE, Raw::MouseButton { n: 4, down: true }), vec![ctl("m", Value::Button(true))]);
    e.input(MOUSE, Raw::MouseButton { n: 4, down: false });
    key(&mut e, ALT, false);

    e.learn(Kind::Button, 1);
    key(&mut e, CTRL, true);
    let out = e.input(MOUSE, Raw::Wheel { dx: 0, dy: 120 });
    let [Out::Learned { binding: b, .. }] = &out[..] else { panic!("{out:?}") };
    assert_eq!((b.trigger, b.modifiers.clone()), (Element::WheelStep(Wheel::Up), vec![Element::Key(CTRL)]));
    e.set_controls(&[(f(), vec![decl("z", Kind::Button, None)])], &store(vec![("z", vec![b.clone()])]));
    assert_eq!(e.input(MOUSE, Raw::Wheel { dx: 0, dy: 120 }), vec![ctl("z", Value::Button(true)), ctl("z", Value::Button(false))]);
    key(&mut e, CTRL, false);
    assert!(e.input(MOUSE, Raw::Wheel { dx: 0, dy: 120 }).is_empty());
}

#[test]
fn axis_chords_follow_their_modifier() {
    // Review 6: "throttle" is axis 0; "zoom" is axis 0 with button 5 held (a HOTAS shift).
    let mut shifted = joy(Element::Axis(0), "p1");
    shifted.modifiers = vec![Element::Button(5)];
    let decls = vec![decl("throttle", Kind::Axis, None), decl("zoom", Kind::Axis, None)];
    let mut e = engine(decls, store(vec![("throttle", vec![joy(Element::Axis(0), "p1")]), ("zoom", vec![shifted])]));
    plug(&mut e, 10, "p1", &[Raw::Axis { index: 0, value: 0 }, Raw::Button { index: 5, down: false }]);
    assert_eq!(e.input(10, Raw::Axis { index: 0, value: 16384 }), vec![ctl("throttle", Value::Axis(16384.0 / 32767.0))]);
    assert_eq!(button(&mut e, 10, 5, true), vec![ctl("zoom", Value::Axis(16384.0 / 32767.0))]);
    assert_eq!(e.input(10, Raw::Axis { index: 0, value: 32767 }), vec![ctl("zoom", Value::Axis(1.0))]);
    assert!(e.tick(5000).is_empty());
    // Shift up: zoom keeps its value, the throttle takes the lever's position.
    assert_eq!(button(&mut e, 10, 5, false), vec![ctl("throttle", Value::Axis(1.0))]);
    assert!(e.tick(5100).is_empty());
    e.set_gate(None);
    assert_eq!(e.set_gate(live()), vec![ctl("throttle", Value::Axis(1.0))]);
}

#[test]
fn wrapped_axes_and_hats() {
    let decls = vec![decl("a", Kind::Axis, None), decl("h", Kind::Button, None)];
    let mut e = engine(decls, store(vec![("a", vec![joy(Element::Axis(0), "p1")]), ("h", vec![joy(Element::Hat(0, HatDir::Left), "p1")])]));
    e.added(10, Source::Joystick, Some(stick("p1")), vec![true, true]);
    e.baseline(10, &[Raw::Axis { index: 0, value: -32768 }, Raw::Hat { index: 0, mask: 0 }]); // wrapped center
    // HIDMaestro before the fix: set 1.0 reads -1 (case 0.7e).
    assert_eq!(e.input(10, Raw::Axis { index: 0, value: -1 }), vec![ctl("a", Value::Axis(1.0))]);
    assert_eq!(e.input(10, Raw::Hat { index: 0, mask: 8 | 1 }), vec![ctl("h", Value::Button(true))]);
    assert_eq!(e.input(10, Raw::Hat { index: 0, mask: 0 }), vec![ctl("h", Value::Button(false))]);
}

#[test]
fn wheel_and_mouse_move() {
    let mut e = engine(vec![decl("zoom", Kind::Delta, Some("mouse:wheel:y")), decl("next", Kind::Button, Some("mouse:wheel:up")), decl("pan", Kind::Delta, Some("mouse:move:x"))], Store::default());
    let out = e.input(MOUSE, Raw::Wheel { dx: 0, dy: 60 });
    assert_eq!(out, vec![ctl("zoom", Value::Delta(0.5))]); // half a notch: no step yet
    let out = e.input(MOUSE, Raw::Wheel { dx: 0, dy: 60 });
    assert_eq!(out, vec![ctl("zoom", Value::Delta(0.5)), ctl("next", Value::Button(true)), ctl("next", Value::Button(false))]);
    assert_eq!(e.input(MOUSE, Raw::Move { dx: -7, dy: 3 }), vec![ctl("pan", Value::Delta(-7.0))]);
}

#[test]
fn learn_keys_chords_buttons_axes_and_mouse() {
    let mut e = engine(vec![], Store::default());
    e.learn(Kind::Button, 1);
    assert!(key(&mut e, CTRL, true).is_empty());
    let out = key(&mut e, R, true);
    let Out::Learned { binding: b, .. } = &out[0] else { panic!("{out:?}") };
    assert_eq!((b.device.clone(), b.trigger, b.modifiers.clone()), (DeviceRef::Any(Source::Keyboard), Element::Key(R), vec![Element::Key(CTRL)]));
    key(&mut e, R, false);
    key(&mut e, CTRL, false);

    e.learn(Kind::Button, 1);
    key(&mut e, CTRL, true);
    let out = key(&mut e, CTRL, false);
    assert!(matches!(&out[..], [Out::Learned { binding: b, .. }] if b.trigger == Element::Key(CTRL) && b.modifiers.is_empty()));

    // A trigger that rests at -1 learns as a 0..1 axis. SDL sent no event for it before the move
    // (it holds back an axis that does not move); its rest comes from the baseline (review 3).
    plug(&mut e, 10, "p1", &[Raw::Axis { index: 4, value: -32768 }]);
    e.learn(Kind::Axis, 1);
    assert!(e.input(10, Raw::Axis { index: 4, value: -20000 }).is_empty());
    let out = e.input(10, Raw::Axis { index: 4, value: 0 });
    assert!(matches!(&out[..], [Out::Learned { binding: b, .. }] if b.trigger == Element::Axis(4) && b.tuning.half && b.device == DeviceRef::Joystick(stick("p1"))));

    e.learn(Kind::Delta, 1);
    assert!(e.input(MOUSE, Raw::Move { dx: 30, dy: 2 }).is_empty());
    let out = e.input(MOUSE, Raw::Move { dx: 30, dy: 2 });
    assert!(matches!(&out[..], [Out::Learned { binding: b, .. }] if b.trigger == Element::Move(MoveAxis::X)));
}

#[test]
fn binding_change_releases_and_axes_resync_on_gate_open() {
    let mut e = engine(vec![decl("t", Kind::Button, Some("r")), decl("a", Kind::Axis, None)], Store::default());
    assert_eq!(key(&mut e, R, true), vec![ctl("t", Value::Button(true))]);
    // The binding of t changes (review R1-B1: only a changed control releases, not all).
    let decls = vec![decl("t", Kind::Button, Some("q")), decl("a", Kind::Axis, None)];
    assert_eq!(e.set_controls(&[(f(), decls)], &store(vec![("a", vec![joy(Element::Axis(0), "p1")])])), vec![ctl("t", Value::Button(false))]);
    let half = Value::Axis(16384.0 / 32767.0);
    assert_eq!(plug(&mut e, 10, "p1", &[Raw::Axis { index: 0, value: 16384 }]), vec![ctl("a", half)]);
    assert_eq!(e.set_gate(None), vec![ctl("a", Value::Axis(0.0))]);
    assert_eq!(e.set_gate(live()), vec![ctl("a", half)]);
}

#[test]
fn a_twin_takeover_drops_sdl_values_before_its_baseline() {
    // Review round 2: after the reopen SDL first sends 0, then the real -1, for an axis at rest.
    let decls = vec![decl("b", Kind::Button, None), decl("a", Kind::Axis, None)];
    let mut e = engine(decls, store(vec![("b", vec![joy(Element::Axis(0), "p1")]), ("a", vec![joy(Element::Axis(1), "p1")])]));
    let half = Value::Axis(16384.0 / 32767.0);
    let snapshot = [Raw::Axis { index: 0, value: -32768 }, Raw::Axis { index: 1, value: 16384 }];
    assert_eq!(plug(&mut e, 10, "p1", &snapshot), vec![ctl("a", half)]);
    e.removed(10, 1000);
    e.added(12, Source::Joystick, Some(stick("p1")), vec![]);
    assert!(e.tick(1100).is_empty()); // the axis control keeps its value while the twin settles
    assert!(e.input(12, Raw::Axis { index: 0, value: 0 }).is_empty());
    assert!(e.input(12, Raw::Axis { index: 0, value: -32768 }).is_empty());
    assert!(e.baseline(12, &snapshot).is_empty());
    assert_eq!(e.input(12, Raw::Axis { index: 0, value: 32767 }), vec![ctl("b", Value::Button(true))]);
}

#[test]
fn mouse_chords_fire_only_the_most_specific_binding() {
    // Review round 2: plain and Ctrl bindings on the same wheel and movement.
    let decls = vec![
        decl("zoom", Kind::Delta, Some("mouse:wheel:y")), decl("fine", Kind::Delta, Some("ctrl+mouse:wheel:y")),
        decl("next", Kind::Button, Some("mouse:wheel:up")), decl("pan", Kind::Delta, Some("mouse:move:x")),
        decl("turn", Kind::Delta, Some("ctrl+mouse:move:x")),
    ];
    let mut e = engine(decls, Store::default());
    key(&mut e, CTRL, true);
    assert_eq!(e.input(MOUSE, Raw::Wheel { dx: 0, dy: 120 }), vec![ctl("fine", Value::Delta(1.0))]);
    assert_eq!(e.input(MOUSE, Raw::Move { dx: 5, dy: 0 }), vec![ctl("turn", Value::Delta(5.0))]);
    key(&mut e, CTRL, false);
    assert_eq!(e.input(MOUSE, Raw::Wheel { dx: 0, dy: 120 }), vec![ctl("zoom", Value::Delta(1.0)), ctl("next", Value::Button(true)), ctl("next", Value::Button(false))]);
    assert_eq!(e.input(MOUSE, Raw::Move { dx: 5, dy: 0 }), vec![ctl("pan", Value::Delta(5.0))]);
}

#[test]
fn the_axis_that_moved_last_drives_the_control() {
    // Review round 2: one control, two levers on two devices.
    let mut e = engine(vec![decl("a", Kind::Axis, None)], store(vec![("a", vec![joy(Element::Axis(0), "p1"), joy(Element::Axis(0), "p2")])]));
    plug(&mut e, 10, "p1", &[Raw::Axis { index: 0, value: 0 }]);
    plug(&mut e, 11, "p2", &[Raw::Axis { index: 0, value: 0 }]);
    assert_eq!(e.input(11, Raw::Axis { index: 0, value: 16384 }), vec![ctl("a", Value::Axis(16384.0 / 32767.0))]);
    assert!(e.tick(5000).is_empty());
    assert_eq!(e.input(10, Raw::Axis { index: 0, value: 32767 }), vec![ctl("a", Value::Axis(1.0))]);
    assert!(e.tick(5100).is_empty());
}

// Slice 4 review round 2: only the features of the session in front get events and compete. A
// longer chord of a feature that runs in another game does not hide this game's shorter chord.
#[test]
fn only_the_features_of_the_game_in_front_get_controls() {
    let mut e = Engine::new();
    e.added(KBD, Source::Keyboard, None, vec![]);
    let r = |id: &str, def: &str| (FeatureRef::new(id, "1"), vec![decl("t", Kind::Button, Some(def))]);
    e.set_controls(&[r("ats_only", "ctrl+r"), r("msfs_only", "ctrl+shift+r")], &Store::default());
    e.set_gate(Some([FeatureRef::new("ats_only", "1")].into()));
    key(&mut e, CTRL, true);
    key(&mut e, 0xE1, true);
    let out = key(&mut e, R, true);
    assert_eq!(out, vec![Out::Control { feature: "ats_only".into(), control: "t".into(), value: Value::Button(true) }]);
    // The front session's feature set changes: what was held goes up.
    assert_eq!(e.set_gate(Some([FeatureRef::new("msfs_only", "1")].into())), vec![Out::Control { feature: "ats_only".into(), control: "t".into(), value: Value::Button(false) }]);
    assert!(e.set_gate(Some([FeatureRef::new("msfs_only", "1")].into())).is_empty(), "same set: no change");
}

// Session sync follow-up (plan 2026-10-19-feature-session-sync.md section 11): after an update,
// a game that runs the old version gets the old version's controls, the other game the new ones.
#[test]
fn the_game_in_front_gets_the_controls_of_the_version_that_it_runs() {
    let mut e = Engine::new();
    e.added(KBD, Source::Keyboard, None, vec![]);
    let (old, new) = (FeatureRef::new("f", "1"), FeatureRef::new("f", "2"));
    // Version 2 renamed "t" to "toggle" on the same key.
    e.set_controls(&[(old.clone(), vec![decl("t", Kind::Button, Some("ctrl+r"))]), (new.clone(), vec![decl("toggle", Kind::Button, Some("ctrl+r"))])], &Store::default());
    e.set_gate(Some([old].into()));
    key(&mut e, CTRL, true);
    assert_eq!(key(&mut e, R, true), vec![ctl("t", Value::Button(true))], "the old version gets only its own control");
    assert_eq!(e.set_gate(Some([new].into())), vec![ctl("t", Value::Button(false))], "another game in front: release first");
    key(&mut e, R, false);
    assert_eq!(key(&mut e, R, true), vec![ctl("toggle", Value::Button(true))], "the new version gets only its own control");
}

// Session sync review R1-B1: another version that starts in a background game adds controls; a
// control that did not change keeps its state, so the held button in the game in front stays down.
#[test]
fn new_controls_of_another_version_keep_a_held_button() {
    let mut e = Engine::new();
    e.added(KBD, Source::Keyboard, None, vec![]);
    let (old, new) = (FeatureRef::new("f", "1"), FeatureRef::new("f", "2"));
    let v1 = (old.clone(), vec![decl("t", Kind::Button, Some("ctrl+r"))]);
    e.set_controls(std::slice::from_ref(&v1), &Store::default());
    e.set_gate(Some([old].into()));
    key(&mut e, CTRL, true);
    assert_eq!(key(&mut e, R, true), vec![ctl("t", Value::Button(true))]);
    let both = [v1.clone(), (new, vec![decl("toggle", Kind::Button, Some("ctrl+r"))])];
    assert!(e.set_controls(&both, &Store::default()).is_empty(), "no release of an unchanged control");
    assert_eq!(key(&mut e, R, false), vec![ctl("t", Value::Button(false))], "still held: its up comes");
    key(&mut e, R, true);
    let changed = [(v1.0.clone(), vec![decl("t", Kind::Button, Some("ctrl+q"))])];
    assert_eq!(e.set_controls(&changed, &Store::default()), vec![ctl("t", Value::Button(false))], "a changed binding releases");
}

// Review R2-B1: a kept hold on a joystick found by its model (port move) must release when a new
// control of another version claims that path: the binding no longer means that device.
#[test]
fn a_new_claim_of_the_path_releases_a_kept_model_match() {
    let (old, new) = (FeatureRef::new("f", "1"), FeatureRef::new("f", "2"));
    let bindings = store(vec![("t", vec![joy(Element::Button(1), "p1")]), ("x", vec![joy(Element::Button(2), "p3")])]);
    let v1 = (old.clone(), vec![decl("t", Kind::Button, None)]);
    let mut e = Engine::new();
    e.set_controls(std::slice::from_ref(&v1), &bindings);
    e.set_gate(Some([old].into()));
    plug(&mut e, 12, "p3", &[]);
    assert_eq!(button(&mut e, 12, 1, true), vec![ctl("t", Value::Button(true))], "p1 moved to p3: model match");
    let both = [v1, (new, vec![decl("x", Kind::Button, None)])];
    assert_eq!(e.set_controls(&both, &bindings), vec![ctl("t", Value::Button(false))], "p3 is now claimed by x");
    assert!(button(&mut e, 12, 1, false).is_empty());
}

// SDL thread finding on R2-B1: in a twin churn the twin that comes back first is live and
// outranks the held twin in its debounce; new controls must not end that hold early.
#[test]
fn new_controls_during_a_twin_churn_keep_a_held_model_match() {
    let (old, new) = (FeatureRef::new("f", "1"), FeatureRef::new("f", "2"));
    let bindings = store(vec![("t", vec![joy(Element::Button(1), "p1")])]);
    let v1 = (old.clone(), vec![decl("t", Kind::Button, None)]);
    let mut e = Engine::new();
    e.set_controls(std::slice::from_ref(&v1), &bindings);
    e.set_gate(Some([old].into()));
    plug(&mut e, 10, "p3", &[]);
    plug(&mut e, 11, "p4", &[]);
    assert_eq!(button(&mut e, 10, 1, true), vec![ctl("t", Value::Button(true))], "p1 moved: model match, lowest id");
    e.removed(10, 2000);
    e.removed(11, 2000);
    e.added(13, Source::Joystick, Some(stick("p4")), vec![]);
    assert!(e.baseline(13, &[]).is_empty());
    let both = [v1, (new, vec![decl("toggle", Kind::Button, Some("ctrl+r"))])];
    assert!(e.set_controls(&both, &bindings).is_empty(), "the held twin is in its debounce, not lost");
    e.added(14, Source::Joystick, Some(stick("p3")), vec![]);
    assert!(e.baseline(14, &[Raw::Button { index: 1, down: true }]).is_empty());
    assert!(e.tick(3500).is_empty());
    assert_eq!(button(&mut e, 14, 1, false), vec![ctl("t", Value::Button(false))]);
}

// Slice 4 review round 1: a click on the app's Cancel button must not become the binding, and a
// result names the learn that found it.
#[test]
fn learn_skips_the_left_mouse_button_and_gives_its_id() {
    let mut e = engine(vec![], Store::default());
    e.learn(Kind::Button, 7);
    assert!(e.input(MOUSE, Raw::MouseButton { n: 1, down: true }).is_empty());
    assert!(e.input(MOUSE, Raw::MouseButton { n: 1, down: false }).is_empty());
    key(&mut e, CTRL, true);
    assert!(e.input(MOUSE, Raw::MouseButton { n: 1, down: true }).is_empty(), "not with a modifier either");
    e.input(MOUSE, Raw::MouseButton { n: 1, down: false });
    key(&mut e, CTRL, false);
    e.learn(Kind::Button, 8);
    let out = e.input(MOUSE, Raw::MouseButton { n: 2, down: true });
    assert!(matches!(&out[..], [Out::Learned { binding: b, id: 8 }] if b.trigger == Element::MouseButton(2)), "{out:?}");
}

#[test]
fn learn_gives_only_a_binding_of_the_asked_kind() {
    // Review round 2: a modifier released alone is a button, not an axis or delta binding.
    let mut e = engine(vec![], Store::default());
    plug(&mut e, 10, "p1", &[Raw::Axis { index: 0, value: 0 }]);
    for kind in [Kind::Axis, Kind::Delta] {
        e.learn(kind, 1);
        assert!(key(&mut e, CTRL, true).is_empty());
        assert!(key(&mut e, CTRL, false).is_empty());
        assert!(e.input(MOUSE, Raw::MouseButton { n: 1, down: true }).is_empty());
        e.input(MOUSE, Raw::MouseButton { n: 1, down: false });
        let out = if kind == Kind::Axis { e.input(10, Raw::Axis { index: 0, value: 32767 }) } else { e.input(MOUSE, Raw::Move { dx: 60, dy: 0 }) };
        let [Out::Learned { binding: b, .. }] = &out[..] else { panic!("{kind:?}: {out:?}") };
        assert!(b.fits(kind), "{b:?}");
        e.input(10, Raw::Axis { index: 0, value: 0 });
    }
}

#[test]
fn learn_a_hotas_shift_button_with_a_hat() {
    // Review round 2: the shift button waits; the hat is the trigger; alone, the button is.
    let mut e = engine(vec![], Store::default());
    plug(&mut e, 10, "p1", &[Raw::Button { index: 5, down: false }, Raw::Hat { index: 0, mask: 0 }]);
    e.learn(Kind::Button, 1);
    assert!(button(&mut e, 10, 5, true).is_empty());
    let out = e.input(10, Raw::Hat { index: 0, mask: 1 });
    let [Out::Learned { binding: b, .. }] = &out[..] else { panic!("{out:?}") };
    assert_eq!((b.trigger, b.modifiers.clone()), (Element::Hat(0, HatDir::Up), vec![Element::Button(5)]));
    e.input(10, Raw::Hat { index: 0, mask: 0 });
    button(&mut e, 10, 5, false);
    e.learn(Kind::Button, 1);
    assert!(button(&mut e, 10, 5, true).is_empty());
    let out = button(&mut e, 10, 5, false);
    assert!(matches!(&out[..], [Out::Learned { binding: b, .. }] if b.trigger == Element::Button(5) && b.modifiers.is_empty()), "{out:?}");
}

#[test]
fn a_takeover_snapshot_is_installed_before_chords_are_checked() {
    // Review round 3: button 5 (shift) went up and button 0 down while the twin settled; the
    // snapshot lists button 0 first. Only the plain binding may fire.
    let mut shifted = joy(Element::Button(0), "p1");
    shifted.modifiers = vec![Element::Button(5)];
    let decls = vec![decl("plain", Kind::Button, None), decl("shifted", Kind::Button, None)];
    let mut e = engine(decls, store(vec![("plain", vec![joy(Element::Button(0), "p1")]), ("shifted", vec![shifted])]));
    plug(&mut e, 10, "p1", &[Raw::Button { index: 0, down: false }, Raw::Button { index: 5, down: false }]);
    assert!(button(&mut e, 10, 5, true).is_empty());
    e.removed(10, 1000);
    e.added(12, Source::Joystick, Some(stick("p1")), vec![]);
    let out = e.baseline(12, &[Raw::Button { index: 0, down: true }, Raw::Button { index: 5, down: false }]);
    assert_eq!(out, vec![ctl("plain", Value::Button(true))]);
}

#[test]
fn learn_follows_a_twin_takeover_and_ignores_a_removed_keyboard() {
    // Review round 3: a held shift button keeps its learn state through a reconnect.
    let mut e = engine(vec![], Store::default());
    plug(&mut e, 10, "p1", &[Raw::Button { index: 5, down: false }]);
    e.learn(Kind::Button, 1);
    assert!(button(&mut e, 10, 5, true).is_empty());
    e.removed(10, 1000);
    e.added(12, Source::Joystick, Some(stick("p1")), vec![]);
    assert!(e.baseline(12, &[Raw::Button { index: 5, down: true }]).is_empty());
    let out = button(&mut e, 12, 5, false);
    assert!(matches!(&out[..], [Out::Learned { binding: b, .. }] if b.trigger == Element::Button(5) && b.device == DeviceRef::Joystick(stick("p1"))), "{out:?}");
    // Ctrl held on a keyboard that is unplugged is not a modifier of the next learned key.
    e.added(KBD2, Source::Keyboard, None, vec![]);
    e.input(KBD2, Raw::Key { usage: CTRL, down: true });
    e.removed(KBD2, 2000);
    e.learn(Kind::Button, 1);
    let out = key(&mut e, R, true);
    assert!(matches!(&out[..], [Out::Learned { binding: b, .. }] if b.trigger == Element::Key(R) && b.modifiers.is_empty()), "{out:?}");
}
