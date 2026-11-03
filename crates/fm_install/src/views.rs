// Widget views (feature surfaces plan, section 4, FS1): the declarative content of a widget that
// the host draws. One view tree per size of the widget and a list of actions, in
// `widgets/<name>.json`. Each node type has a closed set of fields (serde `deny_unknown_fields`),
// so no creator code or markup reaches the home screen. `parse` is the one check for fm_pack,
// the install and the app, which gives the parsed views to its UI.
use crate::feature::Manifest;
use crate::surfaces::{valid_id, Setting, Widget, WidgetSize, IMAGE_EXTS, WIDGETS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;

/// Section 8 of the plan, for one views file.
pub mod limits {
    /// Nodes in one view tree, and its depth (a root node is depth 1).
    pub const NODES: usize = 64;
    pub const DEPTH: usize = 8;
    pub const ACTIONS: usize = 16;
    /// The JSON data of a `send` action (the page message limit of the hosts).
    pub const SEND_DATA: usize = fm_feature_sdk::abi::PAGE_LIMIT;
    pub const TEXT: usize = 80;
    pub const LABEL: usize = 40;
    pub const UNIT: usize = 12;
    pub const POINTER: usize = 256;
    pub const POINTS: u32 = 120;
}

/// The parsed views of one widget: a tree per size, and the actions that its nodes name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Views {
    #[serde(rename = "1x1", default, skip_serializing_if = "Option::is_none")]
    pub small: Option<Node>,
    #[serde(rename = "2x1", default, skip_serializing_if = "Option::is_none")]
    pub wide: Option<Node>,
    #[serde(rename = "2x2", default, skip_serializing_if = "Option::is_none")]
    pub large: Option<Node>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
}

impl Views {
    pub fn of(&self, size: WidgetSize) -> Option<&Node> {
        match size {
            WidgetSize::Small => self.small.as_ref(),
            WidgetSize::Wide => self.wide.as_ref(),
            WidgetSize::Large => self.large.as_ref(),
            WidgetSize::Unknown => None,
        }
    }
}

/// Where a node reads its value: a field of the feature's live messages (`topic` of `Host::ui`
/// and `path`, a JSON Pointer into its data), or `setting`, the member's stored value. Exactly
/// one of the two forms (a struct, so that serde refuses unknown fields).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextStyle {
    Title,
    Body,
    Caption,
}

/// One node of a view tree. The kit of plan section 4; nothing else is drawn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    Stack {
        children: Vec<Node>,
    },
    Row {
        children: Vec<Node>,
    },
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<TextStyle>,
    },
    /// A live or setting value as text: a number with `digits`, else the value as it is.
    Value {
        source: Source,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digits: Option<u8>,
    },
    Gauge {
        source: Source,
        min: f64,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    Bar {
        source: Source,
        min: f64,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// The last `points` values of a live number.
    Sparkline {
        source: Source,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        points: Option<u32>,
    },
    /// A raster image of the package, `widgets/...`.
    Image {
        src: String,
        alt: String,
    },
    /// Shows `state` (true or false) and runs `on` or `off` when the member switches it.
    Toggle {
        label: String,
        state: Source,
        on: String,
        off: String,
    },
    Button {
        label: String,
        action: String,
    },
    Spacer {},
}

/// What a widget can do (section 4): `send` a message to the running feature, set `setting` to
/// `value`, or `open` the feature page; exactly one form. The app runs it from the views file of
/// the version, never from the UI's words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send: Option<Send>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<OpenTarget>,
}

/// The one form of an action.
pub enum Does<'a> {
    Send(&'a Send),
    Setting(&'a str, &'a serde_json::Value),
    Open,
}

impl Action {
    pub fn does(&self) -> Option<Does<'_>> {
        match (&self.send, &self.setting, &self.value, &self.open) {
            (Some(s), None, None, None) => Some(Does::Send(s)),
            (None, Some(id), Some(v), None) => Some(Does::Setting(id, v)),
            (None, None, None, Some(OpenTarget::Page)) => Some(Does::Open),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Send {
    pub topic: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenTarget {
    Page,
}

fn bad(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

/// The topic rule of page messages (fm_feature_sdk host `page`): 1 to 64 of a-z, 0-9, '.', '_', '-'.
pub fn valid_topic(t: &str) -> bool {
    (1..=64).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b))
}

fn text_ok(t: &str, max: usize) -> bool {
    let n = t.chars().count();
    (1..=max).contains(&n) && !t.chars().any(char::is_control) && !t.trim().is_empty()
}

/// The rules of one views file of widget `w` of manifest `m`: every size of the widget has a
/// tree and no other size does; every node in its limits; every action that a node names
/// exists, and every action fits the manifest (a known setting and a value that fits it, a page
/// for `open`). `has_image` says if the package has a widget image.
pub fn parse(bytes: &[u8], w: &Widget, m: &Manifest, has_image: &dyn Fn(&str) -> bool) -> io::Result<Views> {
    let id = &m.id;
    let fail = |why: String| bad(format!("{id}: {}: {why}", w.views));
    let views: Views = serde_json::from_slice(bytes).map_err(|e| fail(format!("not a views file ({e})")))?;
    for size in [WidgetSize::Small, WidgetSize::Wide, WidgetSize::Large] {
        if views.of(size).is_some() != w.sizes.contains(&size) {
            return Err(fail("it needs one view for each size of its widget, and no other".into()));
        }
    }
    if views.actions.len() > limits::ACTIONS {
        return Err(fail(format!("at most {} actions", limits::ACTIONS)));
    }
    let mut actions = BTreeSet::new();
    for a in &views.actions {
        if !valid_id(&a.id) || !actions.insert(a.id.as_str()) {
            return Err(fail(format!("action {:?}: the id must be unique: 1 to 32 of a-z, 0-9 and _", a.id)));
        }
        check_action(a, m).map_err(|why| fail(format!("action {:?}: {why}", a.id)))?;
    }
    let settings: BTreeMap<&str, &Setting> = m.surfaces.settings.iter().map(|s| (s.id.as_str(), s)).collect();
    let cx = Cx { actions: &actions, settings: &settings, has_image };
    for size in &w.sizes {
        let root = views.of(*size).expect("checked above");
        let mut count = 0;
        cx.node(root, 1, &mut count).map_err(|why| fail(format!("view {}: {why}", size_name(*size))))?;
    }
    Ok(views)
}

fn size_name(s: WidgetSize) -> &'static str {
    match s {
        WidgetSize::Small => "1x1",
        WidgetSize::Wide => "2x1",
        WidgetSize::Large => "2x2",
        WidgetSize::Unknown => "?",
    }
}

fn check_action(a: &Action, m: &Manifest) -> Result<(), String> {
    match a.does().ok_or("an action needs exactly one of send, setting with value, or open")? {
        Does::Send(send) => {
            let size = serde_json::to_vec(&send.data).map_or(usize::MAX, |b| b.len());
            if !valid_topic(&send.topic) || size > limits::SEND_DATA {
                return Err(format!("send needs a topic of 1 to 64 of a-z, 0-9, '.', '_' or '-', and data of at most {} bytes", limits::SEND_DATA));
            }
            if !m.capabilities.contains(&fm_feature_sdk::Capability::UiApp) {
                return Err("send needs the ui_app capability (page messages)".into());
            }
            Ok(())
        }
        Does::Setting(setting, value) => match m.surfaces.settings.iter().find(|s| s.id == setting) {
            Some(s) if s.fits(value) => Ok(()),
            _ => Err("setting must name a setting of the manifest and a value that fits it".into()),
        },
        Does::Open => m.surfaces.page.is_some().then_some(()).ok_or_else(|| "open page needs a page in the manifest".into()),
    }
}

struct Cx<'a> {
    actions: &'a BTreeSet<&'a str>,
    settings: &'a BTreeMap<&'a str, &'a Setting>,
    has_image: &'a dyn Fn(&str) -> bool,
}

impl Cx<'_> {
    fn node(&self, n: &Node, depth: usize, count: &mut usize) -> Result<(), String> {
        *count += 1;
        if *count > limits::NODES || depth > limits::DEPTH {
            return Err(format!("at most {} nodes and {} levels", limits::NODES, limits::DEPTH));
        }
        let label = |l: &Option<String>| l.as_deref().is_none_or(|l| text_ok(l, limits::LABEL));
        let unit = |u: &Option<String>| u.as_deref().is_none_or(|u| text_ok(u, limits::UNIT));
        let range = |min: f64, max: f64| min.is_finite() && max.is_finite() && min < max;
        let ok = match n {
            Node::Stack { children } | Node::Row { children } => {
                if children.is_empty() {
                    return Err("a stack or row needs children".into());
                }
                for c in children {
                    self.node(c, depth + 1, count)?;
                }
                true
            }
            Node::Text { text, .. } => text_ok(text, limits::TEXT),
            Node::Value { source, label: l, unit: u, digits } => self.source(source)? && label(l) && unit(u) && digits.is_none_or(|d| d <= 6),
            Node::Gauge { source, min, max, label: l, unit: u } => self.source(source)? && range(*min, *max) && label(l) && unit(u),
            Node::Bar { source, min, max, label: l } => self.source(source)? && range(*min, *max) && label(l),
            Node::Sparkline { source, points } => source.topic.is_some() && self.source(source)? && points.is_none_or(|p| (2..=limits::POINTS).contains(&p)),
            Node::Image { src, alt } => crate::surfaces::path_in(src, WIDGETS, &IMAGE_EXTS) && (self.has_image)(src) && text_ok(alt, limits::LABEL),
            Node::Toggle { label: l, state, on, off } => text_ok(l, limits::LABEL) && self.source(state)? && self.action(on)? && self.action(off)?,
            Node::Button { label: l, action } => text_ok(l, limits::LABEL) && self.action(action)?,
            Node::Spacer {} => true,
        };
        ok.then_some(()).ok_or_else(|| format!("a {} node is out of its limits (text, label, unit, digits 0 to 6, min < max, points 2 to {}, a package image)", kind(n), limits::POINTS))
    }

    fn source(&self, s: &Source) -> Result<bool, String> {
        match (&s.topic, &s.path, &s.setting) {
            (Some(topic), Some(path), None) => Ok(valid_topic(topic) && path.len() <= limits::POINTER && (path.is_empty() || path.starts_with('/'))),
            (None, None, Some(setting)) => self.settings.contains_key(setting.as_str()).then_some(true).ok_or_else(|| format!("no setting {setting:?} in the manifest")),
            _ => Err("a source is a topic with a path, or a setting".into()),
        }
    }

    fn action(&self, id: &str) -> Result<bool, String> {
        self.actions.contains(id).then_some(true).ok_or_else(|| format!("no action {id:?} in the views file"))
    }
}

fn kind(n: &Node) -> &'static str {
    match n {
        Node::Stack { .. } => "stack",
        Node::Row { .. } => "row",
        Node::Text { .. } => "text",
        Node::Value { .. } => "value",
        Node::Gauge { .. } => "gauge",
        Node::Bar { .. } => "bar",
        Node::Sparkline { .. } => "sparkline",
        Node::Image { .. } => "image",
        Node::Toggle { .. } => "toggle",
        Node::Button { .. } => "button",
        Node::Spacer {} => "spacer",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::surfaces::tests::surfaced;
    use serde_json::{json, Value};

    /// A views file for the `speed` widget of `surfaced()` (sizes 1x1 and 2x1) that uses every
    /// kind of node and action.
    pub(crate) fn speed() -> Value {
        json!({
            "1x1": { "type": "stack", "children": [
                { "type": "value", "source": { "topic": "speed", "path": "/kt" }, "unit": "kt", "digits": 0 },
                { "type": "text", "text": "Speed", "style": "caption" }
            ] },
            "2x1": { "type": "row", "children": [
                { "type": "gauge", "source": { "topic": "speed", "path": "/kt" }, "min": 0.0, "max": 400.0, "label": "Speed" },
                { "type": "stack", "children": [
                    { "type": "sparkline", "source": { "topic": "speed", "path": "/kt" }, "points": 60 },
                    { "type": "bar", "source": { "setting": "strength" }, "min": 0.0, "max": 1.0 },
                    { "type": "toggle", "label": "Alert", "state": { "setting": "on" }, "on": "alert_on", "off": "alert_off" },
                    { "type": "button", "label": "Reset", "action": "reset" },
                    { "type": "spacer" },
                    { "type": "button", "label": "Open", "action": "open" }
                ] }
            ] },
            "actions": [
                { "id": "alert_on", "setting": "on", "value": true },
                { "id": "alert_off", "setting": "on", "value": false },
                { "id": "reset", "send": { "topic": "speed.reset", "data": { "all": true } } },
                { "id": "open", "open": "page" }
            ]
        })
    }

    fn run(v: &Value) -> io::Result<Views> {
        let m = surfaced();
        parse(&serde_json::to_vec(v).unwrap(), &m.surfaces.widgets[0], &m, &|p| p == "widgets/dial.png")
    }

    /// `speed()` with the JSON Pointer `at` set to `to` (Null removes the key).
    fn with(at: &str, to: Value) -> Value {
        let mut v = speed();
        let (parent, key) = at.rsplit_once('/').unwrap();
        match (v.pointer_mut(parent).unwrap(), to) {
            (Value::Object(o), Value::Null) => drop(o.remove(key)),
            (Value::Object(o), to) => drop(o.insert(key.into(), to)),
            (Value::Array(a), to) => a[key.parse::<usize>().unwrap()] = to,
            _ => panic!("{at}"),
        }
        v
    }

    #[test]
    fn a_full_views_file_parses_and_round_trips() {
        let views = run(&speed()).unwrap();
        assert!(views.of(WidgetSize::Small).is_some() && views.of(WidgetSize::Wide).is_some() && views.of(WidgetSize::Large).is_none());
        assert_eq!(views.actions.len(), 4);
        assert!(matches!(views.actions[2].does(), Some(Does::Send(_))));
        assert_eq!(serde_json::to_value(&views).unwrap(), speed(), "the app gives the UI the same shape");
    }

    #[test]
    fn closed_shapes() {
        for (at, to) in [
            ("/1x1/children/1/onclick", json!("x")),
            ("/1x1/children/1", json!({ "type": "html", "html": "<b>" })),
            ("/1x1/children/0/source/expr", json!("1+1")),
            ("/actions/2/send", json!({ "topic": "x", "url": "y" })),
            ("/actions/3/open", json!("url")),
            ("/actions/2/script", json!("x")),
            ("/2x2", json!({ "type": "spacer" })),
            ("/2x1", Value::Null),
            ("/extra", json!(1)),
        ] {
            assert!(run(&with(at, to.clone())).is_err(), "{at} = {to}");
        }
    }

    #[test]
    fn sources_and_actions_must_exist_and_fit() {
        for (at, to) in [
            ("/1x1/children/0/source", json!({ "topic": "speed" })),
            ("/1x1/children/0/source", json!({ "topic": "speed", "path": "/kt", "setting": "on" })),
            ("/1x1/children/0/source", json!({ "topic": "Speed!", "path": "/kt" })),
            ("/1x1/children/0/source", json!({ "topic": "speed", "path": "kt" })),
            ("/1x1/children/0/source", json!({ "setting": "missing" })),
            ("/2x1/children/1/children/0/source", json!({ "setting": "strength" })),
            ("/2x1/children/1/children/3/action", json!("missing")),
            ("/actions/0/value", json!("yes")),
            ("/actions/0/setting", json!("missing")),
            ("/actions/0/value", Value::Null),
            ("/actions/1/id", json!("alert_on")),
            ("/actions/1/id", json!("Alert")),
            ("/actions/2/send", json!({ "topic": "speed.reset", "data": "x".repeat(limits::SEND_DATA) })),
            ("/actions/2/open", json!("page")),
        ] {
            assert!(run(&with(at, to.clone())).is_err(), "{at} = {to}");
        }
        let m = surfaced();
        let bytes = serde_json::to_vec(&speed()).unwrap();
        let no_page = Manifest { surfaces: crate::surfaces::Surfaces { page: None, ..m.surfaces.clone() }, ..m.clone() };
        assert!(parse(&bytes, &m.surfaces.widgets[0], &no_page, &|_| false).is_err(), "open needs a page");
        let no_ui = Manifest { capabilities: vec![], ..m.clone() };
        assert!(parse(&bytes, &m.surfaces.widgets[0], &no_ui, &|_| false).is_err(), "send needs ui_app");
    }

    #[test]
    fn node_limits() {
        for (at, to) in [
            ("/1x1/children/1/text", json!("x".repeat(limits::TEXT + 1))),
            ("/1x1/children/1/text", json!(" ")),
            ("/1x1/children/1/text", json!("a\nb")),
            ("/1x1/children/0/unit", json!("x".repeat(limits::UNIT + 1))),
            ("/1x1/children/0/digits", json!(7)),
            ("/2x1/children/0/max", json!(0.0)),
            ("/2x1/children/1/children/0/points", json!(limits::POINTS + 1)),
            ("/1x1/children", json!([])),
            ("/1x1/children/1", json!({ "type": "image", "src": "widgets/none.png", "alt": "Dial" })),
            ("/1x1/children/1", json!({ "type": "image", "src": "art/icon.svg", "alt": "Dial" })),
            ("/1x1/children/1", json!({ "type": "image", "src": "widgets/dial.png", "alt": "" })),
        ] {
            assert!(run(&with(at, to.clone())).is_err(), "{at} = {to}");
        }
        run(&with("/1x1/children/1", json!({ "type": "image", "src": "widgets/dial.png", "alt": "Dial" }))).unwrap();
        // Depth: the root is level 1, so DEPTH - 1 stacks around a spacer fit and one more does not.
        let nest = |n: usize| (0..n).fold(json!({ "type": "spacer" }), |c, _| json!({ "type": "stack", "children": [c] }));
        run(&with("/1x1", nest(limits::DEPTH - 1))).unwrap();
        assert!(run(&with("/1x1", nest(limits::DEPTH))).is_err());
        let wide = |n: usize| json!({ "type": "row", "children": vec![json!({ "type": "spacer" }); n] });
        run(&with("/1x1", wide(limits::NODES - 1))).unwrap();
        assert!(run(&with("/1x1", wide(limits::NODES))).is_err());
        let mut many = speed();
        many["actions"] = json!((0..=limits::ACTIONS).map(|i| json!({ "id": format!("a{i}"), "open": "page" })).collect::<Vec<_>>());
        assert!(run(&many).is_err());
    }
}
