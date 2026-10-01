//! OCR backend (tesseract).
//!
//! OCR is optional: the engine only runs it when the user enables it and a usable language
//! pack is installed. Recognition failures on a single frame are reported as a warning and do
//! not abort the analysis.

use super::AnalysisError;
use std::path::Path;
use std::process::Command;

/// Language packs installed for tesseract, excluding `osd` (orientation/script detection is
/// not a recognition language and cannot be used on its own).
pub fn available_ocr_languages() -> Vec<String> {
    let Ok(output) = Command::new("tesseract").arg("--list-langs").output() else {
        return Vec::new();
    };
    // The header line goes to stderr on some builds and stdout on others; scan both.
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut languages: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            line.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        })
        .filter(|line| *line != "osd")
        .map(str::to_owned)
        .collect();
    languages.sort();
    languages.dedup();
    languages
}

/// Recognizes text in `image` and returns its cleaned lines.
pub fn recognize_text(image: &Path, languages: &str) -> Result<Vec<String>, AnalysisError> {
    let mut command = Command::new("tesseract");
    command.arg(image).arg("stdout");
    if !languages.trim().is_empty() {
        command.args(["-l", languages.trim()]);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(AnalysisError::Command {
            stage: "OCR",
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(normalize_lines(&String::from_utf8_lossy(&output.stdout)))
}

/// Cleans raw OCR output: drops blank and punctuation-only lines, collapses whitespace,
/// removes duplicates and caps the result so a noisy frame cannot flood the VIR.
pub fn normalize_lines(raw: &str) -> Vec<String> {
    const MAX_LINES: usize = 32;
    const MAX_CHARS: usize = 400;

    let mut lines: Vec<String> = Vec::new();
    let mut total_chars = 0usize;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || !line.chars().any(char::is_alphanumeric) {
            continue;
        }
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if lines.iter().any(|existing| existing == &line) {
            continue;
        }
        total_chars += line.chars().count();
        if total_chars > MAX_CHARS || lines.len() >= MAX_LINES {
            break;
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::normalize_lines;

    #[test]
    fn drops_noise_and_duplicates() {
        let raw = "  旭川駅  \n\n---\n旭川駅\nSnow Platform\n  !!!  \n";
        assert_eq!(
            normalize_lines(raw),
            vec!["旭川駅".to_owned(), "Snow Platform".to_owned()]
        );
    }

    #[test]
    fn caps_line_count() {
        let raw = (0..100)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(normalize_lines(&raw).len(), 32);
    }
}
