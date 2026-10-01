use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Resolves a fontconfig family name (or `None` for the default UI font) plus a
/// bold/italic style hint into a concrete, Japanese-capable font file path.
///
/// Falls back progressively: family+style+ja -> family+ja -> generic sans-serif+ja ->
/// a short list of common CJK font file locations.
pub fn resolve_font_file(family: Option<String>, bold: bool, italic: bool) -> Option<PathBuf> {
    resolve_font_file_ref(family.as_deref(), bold, italic)
}

pub fn resolve_font_file_ref(family: Option<&str>, bold: bool, italic: bool) -> Option<PathBuf> {
    let base = family.unwrap_or("sans-serif");
    let style = match (bold, italic) {
        (true, true) => Some("Bold Italic"),
        (true, false) => Some("Bold"),
        (false, true) => Some("Italic"),
        (false, false) => None,
    };

    if let Some(style) = style
        && let Some(path) = fc_match(&format!("{base}:style={style}:lang=ja"))
    {
        return Some(path);
    }
    if let Some(path) = fc_match(&format!("{base}:lang=ja")) {
        return Some(path);
    }
    // The generic fallback is redundant when the requested family already is sans-serif.
    if base != "sans-serif"
        && let Some(path) = fc_match("sans-serif:lang=ja")
    {
        return Some(path);
    }
    fallback_font_paths()
}

/// Resolves one fontconfig pattern. Results are cached for the process: the same family is
/// looked up again by the preview font loader and by export, and each miss used to spawn
/// another `fc-match`.
fn fc_match(pattern: &str) -> Option<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(cached) = guard.get(pattern)
    {
        return cached.clone();
    }
    let resolved = Command::new("fc-match")
        .args(["-f", "%{file}\n", pattern])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| output.lines().next().map(PathBuf::from))
        .filter(|path| path.is_file());
    if let Ok(mut guard) = cache.lock() {
        guard.insert(pattern.to_owned(), resolved.clone());
    }
    resolved
}

fn fallback_font_paths() -> Option<PathBuf> {
    [
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/TTF/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

/// Deterministic egui font-family name for a system family plus weight/style combination.
/// The app registers each used combination on demand, and the preview looks it up by this
/// same name so the live canvas shows the typeface that will be exported.
pub fn preview_font_key(family: &str, bold: bool, italic: bool) -> String {
    match (bold, italic) {
        (false, false) => family.to_owned(),
        (true, false) => format!("{family} Bold"),
        (false, true) => format!("{family} Italic"),
        (true, true) => format!("{family} Bold Italic"),
    }
}

/// Short built-in list used when `fc-list` is unavailable (e.g. minimal containers) and as the
/// immediate picker contents while the real system list is still loading.
pub fn fallback_font_families() -> Vec<String> {
    ["Noto Sans CJK JP", "Noto Serif CJK JP", "sans-serif", "serif", "monospace"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// Lists Japanese-capable font families available on the system, for the text
/// inspector's font picker. Falls back to a short built-in list if `fc-list` is
/// unavailable (e.g. minimal containers).
pub fn list_font_families() -> Vec<String> {
    let output = Command::new("fc-list")
        .args([":lang=ja", "family"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned());

    let mut families: Vec<String> = match output {
        Some(text) => text
            .lines()
            .flat_map(|line| line.split(','))
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect(),
        None => Vec::new(),
    };
    families.sort();
    families.dedup();
    if families.is_empty() {
        families = fallback_font_families();
    }
    families.truncate(300);
    families
}
