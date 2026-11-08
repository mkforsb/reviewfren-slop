//! Font configuration: installed font families, the active UI/code fonts and sizes.
//!
//! The active fonts live in a global so that every view function (and the code
//! widget) can pick them up without threading them through all call sites.

use iced::Font;
use std::collections::{BTreeSet, HashSet};
use std::sync::{Mutex, RwLock};

pub const DEFAULT_CODE_SIZE: f32 = 13.0;

#[derive(Debug, Clone, Copy)]
struct Active {
    ui: Font,
    code: Font,
    code_size: f32,
}

static ACTIVE: RwLock<Active> = RwLock::new(Active { ui: Font::DEFAULT, code: Font::MONOSPACE, code_size: DEFAULT_CODE_SIZE });

/// `Font::with_name` needs a `&'static str`; family names are interned (leaked once each).
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut guard = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let set = guard.get_or_insert_with(HashSet::new);
    if let Some(n) = set.get(name) {
        return n;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// The font for a family name; empty or not installed means "use `fallback`"
/// (otherwise the text engine would pick an arbitrary, possibly proportional, font).
pub fn family(name: &str, fallback: Font) -> Font {
    static KNOWN: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    let name = name.trim();
    if name.is_empty() || !KNOWN.get_or_init(|| installed().all.into_iter().collect()).contains(name) {
        fallback
    } else {
        Font::with_name(intern(name))
    }
}

pub fn set_ui(name: &str) {
    ACTIVE.write().unwrap_or_else(|e| e.into_inner()).ui = family(name, Font::DEFAULT);
}

pub fn set_code(name: &str, size: f32) {
    let mut a = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
    a.code = family(name, Font::MONOSPACE);
    a.code_size = size.clamp(8.0, 32.0);
}

pub fn ui() -> Font {
    ACTIVE.read().unwrap_or_else(|e| e.into_inner()).ui
}

pub fn code() -> Font {
    ACTIVE.read().unwrap_or_else(|e| e.into_inner()).code
}

pub fn code_size() -> f32 {
    ACTIVE.read().unwrap_or_else(|e| e.into_inner()).code_size
}

/// Line height used for code rows at the given font size.
pub fn code_line_height(size: f32) -> f32 {
    (size * 1.54).round()
}

/// UI font with a different weight (keeps the configured family).
pub fn ui_weight(weight: iced::font::Weight) -> Font {
    Font { weight, ..ui() }
}

#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub all: Vec<String>,
    pub monospace: Vec<String>,
}

/// Font families known to iced's font system (system fonts + bundled ones).
pub fn installed() -> Installed {
    let mut all = BTreeSet::new();
    let mut mono = BTreeSet::new();
    let mut fs = iced::advanced::graphics::text::font_system().write().unwrap_or_else(|e| e.into_inner());
    for face in fs.raw().db().faces() {
        if let Some((name, _)) = face.families.first() {
            if name.is_empty() || name.starts_with('.') {
                continue;
            }
            all.insert(name.clone());
            if face.monospaced {
                mono.insert(name.clone());
            }
        }
    }
    Installed { all: all.into_iter().collect(), monospace: mono.into_iter().collect() }
}
