// fm_input: the control input engine of Feature Machine (docs/plans/2026-10-19-control-input.md,
// slice 1). Pure Rust with no OS code, so all rules run in unit tests on every OS: elements, key
// position tables, device identity, controls, bindings and their store, and the engine (chords,
// baseline, twin debounce, focus gate, release rule, learn). fm_input_reader (slice 2) feeds it
// from SDL3, raw input and evdev; `wire` is the line format between that process and the app.
pub mod binding;
pub mod coalesce;
pub mod device;
pub mod element;
pub mod engine;
pub mod scancode;
pub mod wire;

pub use binding::{Binding, ControlDecl, Kind, Store, Tuning};
pub use device::{DeviceRef, Fingerprint, Source};
pub use element::Element;
pub use engine::{DevId, Engine, FeatureRef, Out, Raw, Value};
pub use wire::{DeviceInfo, FeatureControls, FromReader, GameSession, ToReader};
