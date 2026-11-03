// Feature surfaces in feature.json, API 6 (docs/plans/2026-11-02-feature-surfaces.md, sections 1,
// 2, 4 and 8): the page entry, the icon and the banner, widgets and settings. The host draws all
// of them except the page, so each field has a closed shape and a size limit. One check for
// fm_pack and install: `check` for the manifest fields, `check_files` for the package files.
use crate::feature::{safe_rel, Manifest};
use fm_feature_sdk::Capability;
use serde::{Deserialize, Serialize};
use std::io;

/// The first feature API with surfaces.
pub const API: u32 = 6;
/// The first app that reads the surface fields: the next Preview after 0.2.0-preview.4 (review r1:
/// "0.2.0" left out every 0.2.0 Preview). A manifest with one of them must name this version or a
/// newer one in `min_app`, so an older app never shows the feature without its surfaces.
pub const MIN_APP: &str = "0.2.0-preview.5";
/// Package folders: `art` holds the icon and the banner, `widgets` the views and their images.
pub const ART: &str = "art";
pub const WIDGETS: &str = "widgets";
/// The folder under each page base where the host serves its own SDK files (page.js, kit.css,
/// kit.js). A package must not have it, so a page can never replace the SDK.
pub const SDK_DIR: &str = "_fm";

/// Section 8 of the plan. Sizes are bytes, images are pixels.
pub mod limits {
    pub const PAGE_FILE: u64 = 2 * 1024 * 1024;
    pub const UI_TOTAL: u64 = 8 * 1024 * 1024;
    pub const UI_FILES: usize = 200;
    pub const ICON: u64 = 64 * 1024;
    pub const BANNER: u64 = 1024 * 1024;
    pub const BANNER_PX: (u32, u32) = (2048, 1024);
    pub const WIDGET_IMAGE: u64 = 256 * 1024;
    pub const WIDGET_IMAGE_PX: (u32, u32) = (512, 512);
    pub const WIDGETS: usize = 8;
    /// One view tree per size; the views file can hold one for each size of its widget.
    pub const VIEW_PER_SIZE: u64 = 32 * 1024;
    pub const SETTINGS: usize = 32;
    pub const TEXT: usize = 256;
    pub const OPTIONS: usize = 64;
}

/// The surface fields of a manifest (flattened into feature.json). All are optional, so an
/// API 1 to 5 manifest has none of them and stays valid.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Surfaces {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<Page>,
    /// `art/<name>.svg`, shown on the tile and in Discover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `art/<name>.webp`, `.png` or `.jpg`, shown on the tile and in Discover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub widgets: Vec<Widget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<Setting>,
}

impl Surfaces {
    pub fn is_empty(&self) -> bool {
        *self == Surfaces::default()
    }
}

/// The creator page: `entry` is relative to `ui/` (as the old `ui` field). `kit`: the page uses
/// the page kit (section 6); the host serves the SDK files to every page in any case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub entry: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub kit: bool,
}

/// A widget that the host draws from its declarative views (FS1). `views` is
/// `widgets/<name>.json` with one view tree per allowed size.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Widget {
    pub id: String,
    pub name: String,
    pub sizes: Vec<WidgetSize>,
    pub views: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum WidgetSize {
    #[serde(rename = "1x1")]
    Small,
    #[serde(rename = "2x1")]
    Wide,
    #[serde(rename = "2x2")]
    Large,
    /// A size of a newer app: the catalog still loads, and the check refuses this manifest.
    #[serde(other)]
    Unknown,
}

/// One setting of the Manage form (section 2). The app stores the member's value per feature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: SettingKind,
    /// One short line under the control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_if: Option<VisibleIf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingKind {
    Toggle { default: bool },
    Range { min: f64, max: f64, step: f64, default: f64 },
    Number {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        default: f64,
    },
    Select { options: Vec<SelectOption>, default: String },
    Text { default: String },
    /// `#rrggbb`.
    Color { default: String },
    /// A type of a newer app: the catalog still loads, and the check refuses this manifest.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// The setting shows only while another toggle or select setting has this value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisibleIf {
    pub setting: String,
    pub equals: serde_json::Value,
}

fn bad(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

/// Ids of widgets and settings: 1 to 32 of a-z, 0-9 and "_", first a letter (the control id rule).
pub fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.starts_with(|c: char| c.is_ascii_lowercase()) && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Text that the host shows: 1 to `max` characters, no control character.
fn shown(text: &str, max: usize) -> bool {
    let n = text.chars().count();
    (1..=max).contains(&n) && !text.chars().any(char::is_control) && !text.trim().is_empty()
}

/// A package path in `folder` with one of `exts` (lower case), as written in the manifest.
pub(crate) fn path_in(path: &str, folder: &str, exts: &[&str]) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e);
    safe_rel(path).is_ok() && path.split('/').next() == Some(folder) && path.split('/').count() >= 2 && ext.is_some_and(|e| exts.contains(&e))
}

const BANNER_EXTS: [&str; 4] = ["webp", "png", "jpg", "jpeg"];
/// Widget images are raster only, so the pixel limit holds (an SVG names any size; review r1).
pub(crate) const IMAGE_EXTS: [&str; 4] = ["webp", "png", "jpg", "jpeg"];

/// The manifest rules of the surface fields. `Manifest::check` calls it.
pub fn check(m: &Manifest) -> io::Result<()> {
    let s = &m.surfaces;
    if s.is_empty() {
        return Ok(());
    }
    let id = &m.id;
    if m.api < API {
        return Err(bad(format!("{id}: page, icon, banner, widgets and settings need API {API}")));
    }
    if !crate::release::fits_app(Some(MIN_APP), m.min_app.as_deref().unwrap_or("0.0.0")) {
        return Err(bad(format!("{id}: page, icon, banner, widgets and settings need min_app {MIN_APP} or newer")));
    }
    if let Some(p) = &s.page {
        if m.ui.is_some() {
            return Err(bad(format!("{id}: give the page in \"page\" or in \"ui\", not in both")));
        }
        if !m.capabilities.contains(&Capability::UiApp) {
            return Err(bad(format!("{id} has a page but no ui_app capability")));
        }
        if safe_rel(&p.entry).is_err() || !p.entry.ends_with(".html") || p.entry.split('/').next() == Some(SDK_DIR) {
            return Err(bad(format!("{id}: page entry {:?} must be an .html file in ui/, not in ui/{SDK_DIR}", p.entry)));
        }
    }
    if s.icon.as_deref().is_some_and(|p| !path_in(p, ART, &["svg"])) {
        return Err(bad(format!("{id}: the icon must be an .svg file in {ART}/")));
    }
    if s.banner.as_deref().is_some_and(|p| !path_in(p, ART, &BANNER_EXTS)) {
        return Err(bad(format!("{id}: the banner must be a .webp, .png or .jpg file in {ART}/")));
    }
    check_widgets(id, &s.widgets)?;
    check_settings(id, &s.settings)
}

fn check_widgets(id: &str, widgets: &[Widget]) -> io::Result<()> {
    if widgets.len() > limits::WIDGETS {
        return Err(bad(format!("{id}: at most {} widgets", limits::WIDGETS)));
    }
    let mut ids = std::collections::BTreeSet::new();
    for w in widgets {
        let mut sizes = w.sizes.clone();
        sizes.sort();
        sizes.dedup();
        let sizes_ok = !w.sizes.is_empty() && sizes.len() == w.sizes.len() && !w.sizes.contains(&WidgetSize::Unknown);
        if !valid_id(&w.id) || !ids.insert(&w.id) || !shown(&w.name, 40) || !sizes_ok || !path_in(&w.views, WIDGETS, &["json"]) {
            return Err(bad(format!("{id}: bad widget {:?} (a unique id, a name of 1 to 40 characters, sizes 1x1, 2x1 or 2x2 once each, views in {WIDGETS}/*.json)", w.id)));
        }
    }
    Ok(())
}

fn check_settings(id: &str, settings: &[Setting]) -> io::Result<()> {
    if settings.len() > limits::SETTINGS {
        return Err(bad(format!("{id}: at most {} settings", limits::SETTINGS)));
    }
    let mut ids = std::collections::BTreeSet::new();
    for s in settings {
        let fail = |why: &str| bad(format!("{id}: setting {:?}: {why}", s.id));
        if !valid_id(&s.id) || !ids.insert(s.id.as_str()) {
            return Err(fail("the id must be unique: 1 to 32 of a-z, 0-9 and _"));
        }
        if !shown(&s.label, 60) || s.info.as_deref().is_some_and(|i| !shown(i, 200)) {
            return Err(fail("the label needs 1 to 60 characters and the info 1 to 200"));
        }
        check_kind(&s.kind).map_err(fail)?;
    }
    // visible_if names another toggle or select setting and a value that it can have.
    for s in settings {
        let Some(v) = &s.visible_if else { continue };
        let other = settings.iter().find(|o| o.id == v.setting && o.id != s.id);
        let fits = match other.map(|o| &o.kind) {
            Some(SettingKind::Toggle { .. }) => v.equals.is_boolean(),
            Some(SettingKind::Select { options, .. }) => options.iter().any(|o| v.equals.as_str() == Some(o.value.as_str())),
            _ => false,
        };
        if !fits {
            return Err(bad(format!("{id}: setting {:?}: visible_if must name another toggle or select setting and one of its values", s.id)));
        }
    }
    Ok(())
}

/// One rule per type: finite numbers, a default that the control can show.
fn check_kind(kind: &SettingKind) -> Result<(), &'static str> {
    let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
    match kind {
        SettingKind::Toggle { .. } => Ok(()),
        SettingKind::Range { min, max, step, default } => {
            let ok = finite(&[*min, *max, *step, *default]) && min < max && *step > 0.0 && *step <= max - min && (min..=max).contains(&default) && on_step(*default, *min, *step);
            ok.then_some(()).ok_or("a range needs min < max, a step above 0 and not above max - min, and a default from min to max on a step")
        }
        SettingKind::Number { min, max, step, default } => {
            let lo = min.unwrap_or(f64::MIN);
            let hi = max.unwrap_or(f64::MAX);
            let ok = finite(&[lo, hi, *default]) && step.is_none_or(|s| s.is_finite() && s > 0.0 && on_step(*default, min.unwrap_or(0.0), s)) && lo <= hi && (lo..=hi).contains(default);
            ok.then_some(()).ok_or("a number needs finite values, min <= max, a step above 0, and a default from min to max on a step")
        }
        SettingKind::Select { options, default } => {
            let mut values = std::collections::BTreeSet::new();
            let each = options.iter().all(|o| (1..=64).contains(&o.value.len()) && shown(&o.label, 60) && values.insert(o.value.as_str()));
            let ok = (1..=limits::OPTIONS).contains(&options.len()) && each && values.contains(default.as_str());
            ok.then_some(()).ok_or("a select needs 1 to 64 options with unique values (1 to 64 bytes) and labels, and a default that is one of the values")
        }
        SettingKind::Text { default } => (default.len() <= limits::TEXT && !default.chars().any(char::is_control)).then_some(()).ok_or("a text default has at most 256 bytes and no control character"),
        SettingKind::Color { default } => color(default).then_some(()).ok_or("a color default is #rrggbb"),
        SettingKind::Unknown => Err("this version of Feature Machine does not know the setting type"),
    }
}

fn color(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// True when `v` is `from` plus a whole number of `step`. The tolerance covers only the float
/// error of this arithmetic (a few units of the last place of the values, at least 1e-12 step),
/// and is never more than a quarter step, so a large step count cannot accept a value between
/// two steps (Sol r2: 1000000000.5 with step 1).
fn on_step(v: f64, from: f64, step: f64) -> bool {
    let nearest = from + ((v - from) / step).round() * step;
    let tolerance = (8.0 * f64::EPSILON * v.abs().max(from.abs()).max(nearest.abs())).max(1e-12 * step).min(step / 4.0);
    (v - nearest).abs() <= tolerance
}

impl Setting {
    /// The default of the manifest as the JSON value that a feature reads.
    pub fn default_value(&self) -> serde_json::Value {
        match &self.kind {
            SettingKind::Toggle { default } => (*default).into(),
            SettingKind::Range { default, .. } | SettingKind::Number { default, .. } => (*default).into(),
            SettingKind::Select { default, .. } | SettingKind::Text { default } | SettingKind::Color { default } => default.as_str().into(),
            SettingKind::Unknown => serde_json::Value::Null,
        }
    }

    /// True when `value` fits this setting of this version: its type, range and step, one of
    /// its options, at most limits::TEXT bytes of text with no control character, or #rrggbb.
    /// One rule for the member's writes and for each version's projection (section 2).
    pub fn fits(&self, value: &serde_json::Value) -> bool {
        match (&self.kind, value) {
            (SettingKind::Toggle { .. }, serde_json::Value::Bool(_)) => true,
            (SettingKind::Range { min, max, step, .. }, v) => v.as_f64().is_some_and(|v| v.is_finite() && (*min..=*max).contains(&v) && on_step(v, *min, *step)),
            (SettingKind::Number { min, max, step, .. }, v) => v.as_f64().is_some_and(|v| {
                v.is_finite() && min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m) && step.is_none_or(|s| on_step(v, min.unwrap_or(0.0), s))
            }),
            (SettingKind::Select { options, .. }, serde_json::Value::String(v)) => options.iter().any(|o| &o.value == v),
            (SettingKind::Text { .. }, serde_json::Value::String(v)) => v.len() <= limits::TEXT && !v.chars().any(char::is_control),
            (SettingKind::Color { .. }, serde_json::Value::String(v)) => color(v),
            _ => false,
        }
    }
}

/// The values that one version runs (section 2): each setting of its own schema, with the
/// stored value when it fits that setting, else the version's default. The stored values do not
/// change, so a rollback or an update keeps the member's choice for each id that fits.
pub fn project(settings: &[Setting], stored: &serde_json::Map<String, serde_json::Value>) -> serde_json::Map<String, serde_json::Value> {
    settings
        .iter()
        .filter(|s| s.kind != SettingKind::Unknown)
        .map(|s| (s.id.clone(), stored.get(&s.id).filter(|v| s.fits(v)).cloned().unwrap_or_else(|| s.default_value())))
        .collect()
}

/// The files of a package (zip path, size) against its manifest: the `ui/` limits (for every
/// API, also the `_fm` folder), and the `art/` and `widgets/` files, which must be the ones that
/// the manifest names, in their size and pixel limits. `read` gives the bytes of one file (only
/// called for files within their limit). Paths are "/" separated, as in a zip.
pub fn check_files(m: &Manifest, files: &[(String, u64)], read: &mut dyn FnMut(&str) -> io::Result<Vec<u8>>) -> io::Result<()> {
    let id = &m.id;
    let under = |folder: &'static str| files.iter().filter(move |(p, _)| p.split('/').next() == Some(folder) && p.contains('/'));
    let ui: Vec<_> = under(crate::feature::parts::UI).collect();
    let total: u64 = ui.iter().map(|(_, n)| n).sum();
    if ui.len() > limits::UI_FILES || total > limits::UI_TOTAL || ui.iter().any(|(_, n)| *n > limits::PAGE_FILE) {
        return Err(bad(format!("{id}: ui/ has at most {} files, {} MB in all and 2 MB per file", limits::UI_FILES, limits::UI_TOTAL / 1024 / 1024)));
    }
    if ui.iter().any(|(p, _)| p.split('/').nth(1).is_some_and(|d| d.eq_ignore_ascii_case(SDK_DIR))) {
        return Err(bad(format!("{id}: ui/{SDK_DIR} is the folder of the Feature Machine SDK files; a package cannot have it")));
    }
    let s = &m.surfaces;
    let has = |path: &str| files.iter().any(|(p, _)| p == path);
    if let Some(p) = &s.page {
        if !has(&format!("{}/{}", crate::feature::parts::UI, p.entry)) {
            return Err(bad(format!("{id}: ui/{} is missing", p.entry)));
        }
    }
    for (path, size) in under(ART) {
        if s.icon.as_deref() == Some(path.as_str()) {
            if *size > limits::ICON || !is_svg(&read(path)?) {
                return Err(bad(format!("{id}: {path} must be an SVG file of at most 64 KB")));
            }
        } else if s.banner.as_deref() == Some(path.as_str()) {
            if *size > limits::BANNER || !image_fits(path, &read(path)?, limits::BANNER_PX) {
                return Err(bad(format!("{id}: {path} must be a WebP, PNG or JPEG image of its type, at most 1 MB and 2048 x 1024 px")));
            }
        } else {
            return Err(bad(format!("{id}: {path} is not the icon or the banner of the manifest")));
        }
    }
    for art in [&s.icon, &s.banner].into_iter().flatten() {
        if !has(art) {
            return Err(bad(format!("{id}: {art} is missing")));
        }
    }
    for (path, size) in under(WIDGETS) {
        // Two widgets can name one views file: it must pass for each of them, as the app reads
        // it for each (Features::widgets_of_version), so no widget is hidden after the install.
        let users: Vec<_> = s.widgets.iter().filter(|w| w.views == *path).collect();
        if !users.is_empty() {
            // The size first: a file over its limit is never read (pack reads source files).
            if users.iter().any(|w| *size > limits::VIEW_PER_SIZE * w.sizes.len() as u64) {
                return Err(bad(format!("{id}: {path} must be at most 32 KB for each size of its widget")));
            }
            let bytes = read(path)?;
            for w in users {
                // The node and action rules (crate::views); an image must be a package file.
                crate::views::parse(&bytes, w, m, &|img| has(img))?;
            }
        } else if path_in(path, WIDGETS, &IMAGE_EXTS) {
            let ok = *size <= limits::WIDGET_IMAGE && image_fits(path, &read(path)?, limits::WIDGET_IMAGE_PX);
            if !ok {
                return Err(bad(format!("{id}: {path} must be an image of its type, at most 256 KB and 512 x 512 px")));
            }
        } else {
            return Err(bad(format!("{id}: {path} is not a widget views file of the manifest or a widget image")));
        }
    }
    if let Some(w) = s.widgets.iter().find(|w| !has(&w.views)) {
        return Err(bad(format!("{id}: {} is missing", w.views)));
    }
    Ok(())
}

/// The host art of `m` that the app may read for its own UI (the art call, plan section 1):
/// the icon, the banner, or a widget image. Gives the size limit of that file; None: not art.
pub fn art_limit(m: &Manifest, file: &str) -> Option<u64> {
    let s = &m.surfaces;
    if s.icon.as_deref() == Some(file) {
        return Some(limits::ICON);
    }
    if s.banner.as_deref() == Some(file) {
        return Some(limits::BANNER);
    }
    (path_in(file, WIDGETS, &IMAGE_EXTS) && !s.widgets.is_empty()).then_some(limits::WIDGET_IMAGE)
}

/// An SVG file: UTF-8 text with an <svg element. It shows only as an image (no script runs).
fn is_svg(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|t| t.contains("<svg"))
}

/// The image is of the type of its extension and within `max` pixels.
fn image_fits(path: &str, bytes: &[u8], max: (u32, u32)) -> bool {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    match image_size(bytes) {
        Some((kind, w, h)) => kind == ext.replace("jpeg", "jpg") && w >= 1 && h >= 1 && w <= max.0 && h <= max.1,
        None => false,
    }
}

/// The type ("png", "jpg", "webp") and the size in pixels of an image, from its header only.
pub fn image_size(b: &[u8]) -> Option<(&'static str, u32, u32)> {
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let be16 = |i: usize| Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le24 = |i: usize| Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, 0]));
    if b.starts_with(b"\x89PNG\r\n\x1a\n") && b.get(12..16) == Some(b"IHDR") {
        return Some(("png", be32(16)?, be32(20)?));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8 " => Some(("webp", le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" if b.get(20) == Some(&0x2f) => {
                let (b1, b2, b3, b4) = (*b.get(21)? as u32, *b.get(22)? as u32, *b.get(23)? as u32, *b.get(24)? as u32);
                Some(("webp", 1 + (((b2 & 0x3f) << 8) | b1), 1 + (((b4 & 0x0f) << 10) | (b3 << 2) | ((b2 & 0xc0) >> 6))))
            }
            b"VP8X" => Some(("webp", 1 + le24(24)?, 1 + le24(27)?)),
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        // Segments up to the first start-of-frame (SOF0 to SOF15, not DHT C4, JPG C8 or DAC CC).
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xff {
                return None;
            }
            let marker = b[i + 1];
            if marker == 0xff {
                i += 1;
                continue;
            }
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return Some(("jpg", be16(i + 7)?, be16(i + 5)?));
            }
            i += 2 + be16(i + 2)? as usize;
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::feature::{Manifest, FEATURE_API};
    use serde_json::json;

    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0]);
        b
    }

    pub(crate) fn surfaced() -> Manifest {
        let json = serde_json::json!({
            "page": { "entry": "pages/index.html", "kit": true },
            "icon": "art/icon.svg",
            "banner": "art/banner.png",
            "widgets": [{ "id": "speed", "name": "Speed", "sizes": ["1x1", "2x1"], "views": "widgets/speed.json" }],
            "settings": [
                { "id": "on", "type": "toggle", "label": "Alert", "default": true },
                { "id": "strength", "type": "range", "label": "Strength", "min": 0, "max": 1, "step": 0.05, "default": 0.5, "visible_if": { "setting": "on", "equals": true } },
                { "id": "mode", "type": "select", "label": "Mode", "options": [{ "value": "day", "label": "Day" }, { "value": "night", "label": "Night" }], "default": "day" },
                { "id": "limit", "type": "number", "label": "Limit", "min": 0, "default": 90, "info": "km/h" },
                { "id": "name", "type": "text", "label": "Name", "default": "" },
                { "id": "tint", "type": "color", "label": "Tint", "default": "#ff8800", "visible_if": { "setting": "mode", "equals": "night" } }
            ]
        });
        Manifest { api: FEATURE_API, min_app: Some(MIN_APP.into()), ui: None, surfaces: serde_json::from_value(json).unwrap(), ..crate::feature::tests::manifest() }
    }

    fn files(m: &Manifest) -> Vec<(String, Vec<u8>)> {
        let mut out = vec![("ui/pages/index.html".to_string(), b"<p>".to_vec()), ("art/icon.svg".into(), b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec()), ("art/banner.png".into(), png(1200, 400)), ("widgets/speed.json".into(), serde_json::to_vec(&crate::views::tests::speed()).unwrap())];
        out.retain(|(p, _)| p != "art/banner.png" || m.surfaces.banner.is_some());
        out
    }

    fn check_with(m: &Manifest, list: &[(String, Vec<u8>)]) -> io::Result<()> {
        let sizes: Vec<(String, u64)> = list.iter().map(|(p, b)| (p.clone(), b.len() as u64)).collect();
        check_files(m, &sizes, &mut |p| Ok(list.iter().find(|(q, _)| q == p).unwrap().1.clone()))
    }

    #[test]
    fn surface_fields_round_trip_and_pass() {
        let m = surfaced();
        m.check().unwrap();
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""page":{"entry":"pages/index.html","kit":true}"#) && json.contains(r#""type":"range""#) && json.contains(r#""sizes":["1x1","2x1"]"#), "{json}");
        assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), m);
        let old = serde_json::to_string(&crate::feature::tests::manifest()).unwrap();
        for f in ["page", "icon", "banner", "widgets", "settings"] {
            assert!(!old.contains(&format!("\"{f}\"")), "an API 5 manifest writes no {f}: {old}");
        }
        check_with(&m, &files(&m)).unwrap();
    }

    #[test]
    fn surface_fields_need_api_6_and_min_app() {
        let m = surfaced();
        assert!(Manifest { api: 5, ..m.clone() }.check().is_err());
        assert!(Manifest { min_app: None, ..m.clone() }.check().is_err());
        assert!(Manifest { min_app: Some("0.2.0-preview.4".into()), ..m.clone() }.check().is_err(), "a preview before the first app");
        Manifest { min_app: Some("0.3.0".into()), ..m.clone() }.check().unwrap();
        let api6_plain = Manifest { api: FEATURE_API, ..crate::feature::tests::manifest() };
        api6_plain.check().unwrap();
    }

    #[test]
    fn page_rules() {
        let m = surfaced();
        let page = |entry: &str| Manifest { surfaces: Surfaces { page: Some(Page { entry: entry.into(), kit: false }), ..m.surfaces.clone() }, ..m.clone() };
        for bad in ["../x.html", "pages/index.js", "_fm/1/page.html", "", "/index.html"] {
            assert!(page(bad).check().is_err(), "{bad:?}");
        }
        assert!(Manifest { ui: Some("index.html".into()), ..m.clone() }.check().is_err(), "ui and page");
        assert!(Manifest { capabilities: vec![Capability::VarsRead], ..m.clone() }.check().is_err(), "page needs ui_app");
        let missing: Vec<_> = files(&m).into_iter().filter(|(p, _)| !p.starts_with("ui/")).collect();
        assert!(check_with(&m, &missing).is_err(), "entry file missing");
    }

    #[test]
    fn art_rules() {
        let m = surfaced();
        let with = |icon: Option<&str>, banner: Option<&str>| Manifest { surfaces: Surfaces { icon: icon.map(Into::into), banner: banner.map(Into::into), ..m.surfaces.clone() }, ..m.clone() };
        for (icon, banner) in [(Some("icon.svg"), None), (Some("art/icon.png"), None), (Some("art/../icon.svg"), None), (None, Some("art/banner.gif")), (None, Some("ui/banner.png"))] {
            assert!(with(icon, banner).check().is_err(), "{icon:?} {banner:?}");
        }
        let swap = |path: &str, bytes: Vec<u8>| files(&m).into_iter().map(|(p, b)| if p == path { (p, bytes.clone()) } else { (p, b) }).collect::<Vec<_>>();
        assert!(check_with(&m, &swap("art/banner.png", png(2049, 400))).is_err(), "too wide");
        assert!(check_with(&m, &swap("art/banner.png", png(2048, 1025))).is_err(), "too high");
        check_with(&m, &swap("art/banner.png", png(2048, 1024))).unwrap();
        assert!(check_with(&m, &swap("art/banner.png", b"<html>".to_vec())).is_err(), "not a PNG");
        assert!(check_with(&m, &swap("art/icon.svg", b"<html>".to_vec())).is_err(), "not an SVG");
        assert!(check_with(&m, &swap("art/icon.svg", [b"<svg>".as_slice(), vec![b' '; 64 * 1024].as_slice()].concat())).is_err(), "icon too large");
        let mut extra = files(&m);
        extra.push(("art/other.png".into(), png(1, 1)));
        assert!(check_with(&m, &extra).is_err(), "art that the manifest does not name");
        let no_icon: Vec<_> = files(&m).into_iter().filter(|(p, _)| p != "art/icon.svg").collect();
        assert!(check_with(&m, &no_icon).is_err(), "declared icon missing");
    }

    #[test]
    fn widget_rules() {
        let m = surfaced();
        let widget = |f: &dyn Fn(&mut Widget)| {
            let mut m = m.clone();
            f(&mut m.surfaces.widgets[0]);
            m
        };
        assert!(widget(&|w| w.sizes = vec![]).check().is_err());
        assert!(widget(&|w| w.sizes = vec![WidgetSize::Small, WidgetSize::Small]).check().is_err());
        assert!(widget(&|w| w.views = "speed.json".into()).check().is_err());
        assert!(widget(&|w| w.id = "Speed".into()).check().is_err());
        assert!(widget(&|w| w.name = "".into()).check().is_err());
        let unknown: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap().replace("\"2x1\"", "\"4x4\"")).unwrap();
        assert_eq!(unknown.surfaces.widgets[0].sizes, [WidgetSize::Small, WidgetSize::Unknown], "a newer size still parses");
        assert!(unknown.check().is_err());
        let many = Manifest { surfaces: Surfaces { widgets: (0..9).map(|i| Widget { id: format!("w{i}"), ..m.surfaces.widgets[0].clone() }).collect(), ..m.surfaces.clone() }, ..m.clone() };
        assert!(many.check().is_err());
        let mut list = files(&m);
        list.push(("widgets/dial.png".into(), png(512, 512)));
        check_with(&m, &list).unwrap();
        list.push(("widgets/big.png".into(), png(513, 10)));
        assert!(check_with(&m, &list).is_err(), "widget image too large");
        let mut list = files(&m);
        list.push(("widgets/run.js".into(), b"x".to_vec()));
        assert!(check_with(&m, &list).is_err(), "not a views file or an image");
        let mut list = files(&m);
        list.push(("widgets/big.svg".into(), b"<svg width='10000' height='10000'/>".to_vec()));
        assert!(check_with(&m, &list).is_err(), "no SVG widget image: its size has no limit");
        assert_eq!(art_limit(&m, "widgets/big.svg"), None);
        assert_eq!(art_limit(&m, "widgets/dial.png"), Some(limits::WIDGET_IMAGE));
        let bad_json: Vec<_> = files(&m).into_iter().map(|(p, b)| if p == "widgets/speed.json" { (p, b"[1]".to_vec()) } else { (p, b) }).collect();
        assert!(check_with(&m, &bad_json).is_err(), "views must be an object");
        // The package check runs the views rules (crate::views): an image node needs its file.
        let mut img = crate::views::tests::speed();
        img["1x1"]["children"][1] = json!({ "type": "image", "src": "widgets/dial.png", "alt": "Dial" });
        let mut list: Vec<_> = files(&m).into_iter().map(|(p, b)| if p == "widgets/speed.json" { (p, serde_json::to_vec(&img).unwrap()) } else { (p, b) }).collect();
        assert!(check_with(&m, &list).is_err(), "image node without its file");
        list.push(("widgets/dial.png".into(), png(64, 64)));
        check_with(&m, &list).unwrap();
        // Sol r3: a views file that two widgets name must pass for both (here the second one
        // offers other sizes than the views of the file; the same sizes pass).
        let mut shared = m.clone();
        let mut big = shared.surfaces.widgets[0].clone();
        big.id = "big".into();
        big.sizes = vec![WidgetSize::Large];
        shared.surfaces.widgets.push(big);
        assert!(check_with(&shared, &files(&shared)).is_err(), "the second widget of a shared file is checked");
        shared.surfaces.widgets[1].sizes = shared.surfaces.widgets[0].sizes.clone();
        check_with(&shared, &files(&shared)).unwrap();
        // Sol r4: a views file over its limit is refused before it is read.
        let list = files(&m);
        let sizes: Vec<(String, u64)> = list.iter().map(|(p, b)| (p.clone(), if p == "widgets/speed.json" { 1 << 30 } else { b.len() as u64 })).collect();
        let err = check_files(&m, &sizes, &mut |p| {
            assert_ne!(p, "widgets/speed.json", "an oversized views file is never read");
            Ok(list.iter().find(|(q, _)| q == p).unwrap().1.clone())
        });
        assert!(err.unwrap_err().to_string().contains("at most 32 KB"));
    }

    #[test]
    fn setting_rules() {
        let m = surfaced();
        let one = |json: serde_json::Value| -> io::Result<()> {
            let s: Setting = serde_json::from_value(json).unwrap();
            Manifest { surfaces: Surfaces { settings: vec![s], ..m.surfaces.clone() }, ..m.clone() }.check()
        };
        use serde_json::json;
        one(json!({ "id": "a", "type": "toggle", "label": "A", "default": false })).unwrap();
        for bad in [
            json!({ "id": "A", "type": "toggle", "label": "A", "default": false }),
            json!({ "id": "a", "type": "toggle", "label": "", "default": false }),
            json!({ "id": "a", "type": "range", "label": "A", "min": 1, "max": 0, "step": 0.1, "default": 0.5 }),
            json!({ "id": "a", "type": "range", "label": "A", "min": 0, "max": 1, "step": 0, "default": 0.5 }),
            json!({ "id": "a", "type": "range", "label": "A", "min": 0, "max": 1, "step": 0.1, "default": 2 }),
            json!({ "id": "a", "type": "number", "label": "A", "min": 5, "default": 1 }),
            json!({ "id": "a", "type": "select", "label": "A", "options": [], "default": "x" }),
            json!({ "id": "a", "type": "select", "label": "A", "options": [{ "value": "x", "label": "X" }, { "value": "x", "label": "Y" }], "default": "x" }),
            json!({ "id": "a", "type": "select", "label": "A", "options": [{ "value": "x", "label": "X" }], "default": "y" }),
            json!({ "id": "a", "type": "text", "label": "A", "default": "x".repeat(257) }),
            json!({ "id": "a", "type": "color", "label": "A", "default": "red" }),
            json!({ "id": "a", "type": "slider3d", "label": "A" }),
            json!({ "id": "a", "type": "toggle", "label": "A", "default": false, "visible_if": { "setting": "a", "equals": true } }),
        ] {
            assert!(one(bad.clone()).is_err(), "{bad}");
        }
        let unknown: Setting = serde_json::from_value(json!({ "id": "a", "type": "slider3d", "label": "A", "axes": 3 })).unwrap();
        assert_eq!(unknown.kind, SettingKind::Unknown, "a newer type still parses");
        let mut s = m.clone();
        s.surfaces.settings[1].visible_if = Some(VisibleIf { setting: "on".into(), equals: json!("yes") });
        assert!(s.check().is_err(), "a toggle value is true or false");
        s.surfaces.settings[1].visible_if = Some(VisibleIf { setting: "mode".into(), equals: json!("dusk") });
        assert!(s.check().is_err(), "a select value is one of its options");
        let many = Manifest { surfaces: Surfaces { settings: (0..33).map(|i| Setting { id: format!("s{i}"), visible_if: None, ..m.surfaces.settings[0].clone() }).collect(), ..m.surfaces.clone() }, ..m.clone() };
        assert!(many.check().is_err());
    }

    /// The ui/ limits hold for every API: count, total, per file, and no _fm folder.
    #[test]
    fn ui_limits() {
        let m = crate::feature::tests::manifest();
        let big = |n: u64| vec![(String::from("ui/index.html"), n)];
        let ok = |list: &[(String, u64)]| check_files(&m, list, &mut |_| Ok(vec![]));
        ok(&big(limits::PAGE_FILE)).unwrap();
        assert!(ok(&big(limits::PAGE_FILE + 1)).is_err());
        let many: Vec<_> = (0..=limits::UI_FILES).map(|i| (format!("ui/{i}.js"), 1)).collect();
        assert!(ok(&many).is_err());
        let total: Vec<_> = (0..5).map(|i| (format!("ui/{i}.js"), limits::PAGE_FILE)).collect();
        assert!(ok(&total).is_err());
        assert!(ok(&[("ui/_FM/1/page.js".into(), 1)]).is_err());
        assert!(ok(&[("art/icon.svg".into(), 1)]).is_err(), "art without a manifest field");
    }

    /// The package path (inspect: consent, direct install and unpack): an API 6 package with its
    /// art and widgets passes; a ui/_fm folder, an unnamed art file or an API 5 manifest with
    /// art is refused; an API 5 package with a ui page still passes.
    #[test]
    fn packages_with_surfaces() {
        let lib = crate::feature::parts::library();
        let zip = |m: &Manifest, extra: &[(&str, &[u8])]| {
            let json = serde_json::to_vec(m).unwrap();
            let base = files(m);
            let mut all: Vec<(&str, &[u8])> = vec![(crate::FEATURE_MANIFEST, &json), (&lib, b"x")];
            all.extend(base.iter().map(|(p, b)| (p.as_str(), b.as_slice())));
            all.extend_from_slice(extra);
            crate::feature::inspect(io::Cursor::new(crate::tests::zip_of(&all)))
        };
        let m = surfaced();
        let (_, parts) = zip(&m, &[]).unwrap();
        assert_eq!(parts, ["art", "bin", "ui", "widgets"]);
        assert!(zip(&m, &[("ui/_fm/1/page.js", b"x")]).is_err());
        assert!(zip(&m, &[("art/extra.svg", b"<svg/>")]).is_err());
        let old = Manifest { api: 5, min_app: None, surfaces: Surfaces::default(), ui: Some("index.html".into()), ..m.clone() };
        let json = serde_json::to_vec(&old).unwrap();
        crate::feature::inspect(io::Cursor::new(crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &json), (&lib, b"x"), ("ui/index.html", b"x")]))).unwrap();
        assert!(crate::feature::inspect(io::Cursor::new(crate::tests::zip_of(&[(crate::FEATURE_MANIFEST, &json), (&lib, b"x"), ("art/icon.svg", b"<svg/>")]))).is_err());
    }

    #[test]
    fn image_headers() {
        assert_eq!(image_size(&png(300, 200)), Some(("png", 300, 200)));
        // JPEG: SOI, APP0 (len 16), SOF0 with height 400 and width 1200.
        let mut jpg = vec![0xff, 0xd8, 0xff, 0xe0, 0, 16];
        jpg.extend([0; 14]);
        jpg.extend([0xff, 0xc0, 0, 17, 8, 0x01, 0x90, 0x04, 0xb0, 3]);
        assert_eq!(image_size(&jpg), Some(("jpg", 1200, 400)));
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        vp8x.extend([0xaf, 0x04, 0x00, 0x8f, 0x01, 0x00]);
        assert_eq!(image_size(&vp8x), Some(("webp", 1200, 400)));
        let mut vp8 = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0".to_vec();
        vp8.extend([0, 0, 0, 0x9d, 0x01, 0x2a, 0xb0, 0x04, 0x90, 0x01]);
        assert_eq!(image_size(&vp8), Some(("webp", 1200, 400)));
        // VP8L: width-1 = 1199 (14 bits), height-1 = 399 (14 bits), packed little-endian.
        let bits: u32 = 1199 | (399 << 14);
        let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
        vp8l.extend(bits.to_le_bytes());
        assert_eq!(image_size(&vp8l), Some(("webp", 1200, 400)));
        assert_eq!(image_size(b"GIF89a"), None);
        assert!(!image_fits("art/b.jpg", &png(10, 10), (100, 100)), "type must match the extension");
        assert!(image_fits("art/b.jpeg", &jpg, (2048, 1024)));
    }

    // Section 2: a value fits only its own type and limits; each version gets its own projection
    // of the stored values (a value that does not fit gives that version's default).
    #[test]
    fn values_fit_their_setting_and_each_version_gets_its_projection() {
        let set = |json: serde_json::Value| -> Vec<Setting> { serde_json::from_value(json).unwrap() };
        let v1 = set(json!([
            { "id": "on", "label": "On", "type": "toggle", "default": true },
            { "id": "strength", "label": "Strength", "type": "range", "min": 0, "max": 1, "step": 0.05, "default": 0.5 },
            { "id": "mode", "label": "Mode", "type": "select", "options": [{ "value": "day", "label": "Day" }, { "value": "dusk", "label": "Dusk" }], "default": "day" },
            { "id": "count", "label": "Count", "type": "number", "min": 1, "step": 1, "default": 3 },
            { "id": "name", "label": "Name", "type": "text", "default": "" },
            { "id": "tint", "label": "Tint", "type": "color", "default": "#ffffff" }
        ]));
        let fits = |id: &str, v: serde_json::Value| v1.iter().find(|s| s.id == id).unwrap().fits(&v);
        assert!(fits("on", json!(false)) && !fits("on", json!(1)));
        assert!(fits("strength", json!(0.35)) && fits("strength", json!(1)) && !fits("strength", json!(0.33)) && !fits("strength", json!(1.05)) && !fits("strength", json!("0.5")));
        assert!(fits("mode", json!("dusk")) && !fits("mode", json!("night")));
        assert!(fits("count", json!(1000)) && !fits("count", json!(0)) && !fits("count", json!(2.5)));
        assert!(fits("name", json!("Leg 1")) && !fits("name", json!("a
b")) && !fits("name", json!("x".repeat(limits::TEXT + 1))));
        assert!(fits("tint", json!("#00ff7F")) && !fits("tint", json!("green")));
        let stored: serde_json::Map<String, serde_json::Value> = serde_json::from_value(json!({ "strength": 0.8, "mode": "dusk", "gone": 1 })).unwrap();
        let p1 = project(&v1, &stored);
        assert_eq!(serde_json::Value::Object(p1), json!({ "on": true, "strength": 0.8, "mode": "dusk", "count": 3.0, "name": "", "tint": "#ffffff" }));
        // An older version: a narrower range and no "dusk" option. It gets its defaults there.
        let v0 = set(json!([
            { "id": "strength", "label": "Strength", "type": "range", "min": 0, "max": 0.5, "step": 0.1, "default": 0.2 },
            { "id": "mode", "label": "Mode", "type": "select", "options": [{ "value": "day", "label": "Day" }], "default": "day" }
        ]));
        assert_eq!(serde_json::Value::Object(project(&v0, &stored)), json!({ "strength": 0.2, "mode": "day" }));
    }

    // Review r2 (slice 3): a large step count does not widen the step tolerance, and every
    // setting that the check accepts takes its own default (the member can save what Manage
    // shows). A default between two steps is refused at the check.
    #[test]
    fn steps_are_exact_and_each_accepted_default_fits() {
        let one = |json: serde_json::Value| -> Setting { serde_json::from_value(json).unwrap() };
        let count = one(json!({ "id": "n", "label": "N", "type": "number", "step": 1, "default": 0 }));
        assert!(count.fits(&json!(1000000000.0)) && !count.fits(&json!(1000000000.5)) && !count.fits(&json!(0.5)));
        let fine = one(json!({ "id": "f", "label": "F", "type": "range", "min": 0, "max": 1, "step": 0.001, "default": 0.5 }));
        assert!(fine.fits(&json!(0.123)) && fine.fits(&json!(0.999)) && !fine.fits(&json!(0.1235)));
        let ok = [
            json!({ "id": "a", "label": "A", "type": "range", "min": 0, "max": 1, "step": 0.05, "default": 0.35 }),
            json!({ "id": "b", "label": "B", "type": "range", "min": -2, "max": 2, "step": 0.1, "default": 0.3 }),
            json!({ "id": "c", "label": "C", "type": "number", "min": 1, "step": 1, "default": 3 }),
            json!({ "id": "d", "label": "D", "type": "number", "default": 2.5 }),
            json!({ "id": "e", "label": "E", "type": "toggle", "default": true }),
            json!({ "id": "f", "label": "F", "type": "select", "options": [{ "value": "x", "label": "X" }], "default": "x" }),
            json!({ "id": "g", "label": "G", "type": "text", "default": "Leg" }),
            json!({ "id": "h", "label": "H", "type": "color", "default": "#a0b0c0" }),
        ];
        for j in ok {
            let s = one(j.clone());
            assert!(check_kind(&s.kind).is_ok() && s.fits(&s.default_value()), "{j}");
        }
        for j in [
            json!({ "id": "a", "label": "A", "type": "range", "min": 0, "max": 1, "step": 0.2, "default": 0.5 }),
            json!({ "id": "c", "label": "C", "type": "number", "min": 1, "step": 2, "default": 2 }),
        ] {
            assert!(check_kind(&one(j.clone()).kind).is_err(), "{j}");
        }
    }

    // Review r1 (slice 3): the largest valid projection must fit the host limit, or a host
    // drops values that the app accepted. Worst case: the most settings, the longest ids, and
    // text of only quotes (each one escapes to two bytes; control characters do not fit).
    #[test]
    fn the_largest_valid_projection_fits_the_host_limit() {
        let schema: Vec<Setting> = (0..limits::SETTINGS)
            .map(|i| serde_json::from_value(json!({ "id": format!("s{i:0>31}"), "label": "Text", "type": "text", "default": "" })).unwrap())
            .collect();
        assert!(schema.iter().all(|s| s.id.len() == 32));
        let text = "\"".repeat(limits::TEXT);
        let stored = schema.iter().map(|s| (s.id.clone(), json!(text))).collect();
        assert!(schema.iter().all(|s| s.fits(&json!(text))));
        let p = serde_json::Value::Object(project(&schema, &stored)).to_string();
        assert!(p.len() > 16 * 1024 && fm_feature_sdk::settings::valid_values(&p), "{} bytes", p.len());
    }
}
