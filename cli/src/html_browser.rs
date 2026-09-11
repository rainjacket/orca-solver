//! Self-contained HTML solution browser generator.
//!
//! Generates a single HTML file with embedded CSS and JS that lets users
//! browse, group, and vote on crossword fill solutions.

use anyhow::{Context, Result};

/// Escape a string for embedding in JSON.
fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn solutions_to_json(solutions: &[(String, Vec<String>)]) -> String {
    let mut json = String::from("[");
    for (i, (grid_text, _)) in solutions.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&escape_json_string(grid_text));
    }
    json.push(']');
    json
}

/// Generate a self-contained HTML solution browser file.
pub fn generate_html_browser(
    solutions: &[(String, Vec<String>)],
    grid_rows: usize,
    grid_cols: usize,
    output_path: &std::path::Path,
) -> Result<()> {
    let json_solutions = solutions_to_json(solutions);

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Orca Solutions ({count} solutions)</title>
<style>
{css}
</style>
</head>
<body>
<header>
  <h1>Orca Solutions</h1>
  <div id="summary"></div>
</header>
<nav class="review-controls" aria-label="Solution review">
  <button id="prev-page">Previous</button>
  <span id="page-status" aria-live="polite"></span>
  <button id="next-page">Next</button>
  <button id="export-marks">Export marks</button>
  <label>Import marks <input id="import-marks" type="file" accept="application/json"></label>
</nav>
<p id="storage-status" role="status"></p>
<main id="groups"></main>
<script>
const SOLUTIONS = {json};
const GRID_ROWS = {rows};
const GRID_COLS = {cols};
{js}
</script>
</body>
</html>"#,
        count = solutions.len(),
        css = HTML_CSS,
        json = json_solutions,
        rows = grid_rows,
        cols = grid_cols,
        js = HTML_JS,
    );

    std::fs::write(output_path, html).with_context(|| {
        format!(
            "Failed to write solution browser: {}",
            output_path.display()
        )
    })?;
    Ok(())
}

const HTML_CSS: &str = include_str!("browser/browser.css");

const HTML_JS: &str = include_str!("browser/browser.js");
