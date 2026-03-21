//! Self-contained HTML solution browser generator.
//!
//! Generates a single HTML file with embedded CSS and JS that lets users
//! browse, group, and vote on crossword fill solutions.

use anyhow::Result;

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

    std::fs::write(output_path, html)?;
    Ok(())
}

const HTML_CSS: &str = r##"
:root {
  --bg-primary: #0d1117;
  --bg-secondary: #161b22;
  --bg-tertiary: #21262d;
  --border: #30363d;
  --text-primary: #c9d1d9;
  --text-secondary: #8b949e;
  --text-tertiary: #484f58;
  --accent: #58a6ff;
  --accent-emphasis: #1f6feb;
  --green: #3fb950;
  --green-emphasis: #238636;
  --yellow: #d29922;
  --red: #f85149;
  --red-emphasis: #da3633;
  --purple: #8957e5;
  --cell-dark: #1c2028;
}

* { margin: 0; padding: 0; box-sizing: border-box; }

body {
  background: var(--bg-primary);
  color: var(--text-primary);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
  padding: 1.5rem;
  line-height: 1.5;
}

header {
  margin-bottom: 1.5rem;
  padding-bottom: 1rem;
  border-bottom: 1px solid var(--border);
}

header h1 {
  font-size: 1.5rem;
  font-weight: 600;
  margin-bottom: 0.5rem;
}

#summary {
  font-size: 0.875rem;
  color: var(--text-secondary);
}

#groups {
  display: flex;
  flex-wrap: wrap;
  gap: 0.75rem;
  align-items: flex-start;
}

.sg-group {
  display: inline-block;
  border: 2px solid transparent;
  border-radius: 6px;
  padding: 6px;
  transition: border-color 0.15s, opacity 0.15s;
  background: var(--bg-secondary);
  vertical-align: top;
}

.sg-group-good {
  border-color: var(--green);
}

.sg-group-bad {
  opacity: 0.4;
}

.solution-grid {
  display: inline-grid;
  gap: 1px;
  background: var(--border);
  border: 2px solid var(--border);
  border-radius: 4px;
  overflow: hidden;
}

.sg-cell {
  width: 24px;
  height: 24px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-family: "SF Mono", "Fira Code", Menlo, Consolas, monospace;
  font-size: 0.75rem;
  font-weight: 500;
}

.sg-black { background: var(--cell-dark); }
.sg-letter {
  background: var(--text-primary);
  color: var(--bg-primary);
}
.sg-variant {
  background: var(--yellow);
  color: var(--bg-primary);
  position: relative;
  cursor: help;
}
.sg-variant[data-variants]::after {
  content: attr(data-variants);
  position: absolute;
  bottom: calc(100% + 4px);
  left: 50%;
  transform: translateX(-50%);
  background: var(--bg-tertiary);
  color: var(--text-primary);
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 2px 6px;
  font-size: 0.6875rem;
  white-space: nowrap;
  pointer-events: none;
  opacity: 0;
  transition: opacity 0.1s;
  z-index: 10;
}
.sg-variant[data-variants]:hover::after {
  opacity: 1;
}
.sg-empty {
  background: var(--bg-tertiary);
}

.sg-info {
  margin-top: 4px;
  display: flex;
  align-items: center;
  gap: 0.5rem;
  min-height: 20px;
}

.sg-count-badge {
  font-size: 0.75rem;
  color: var(--yellow);
  font-weight: 600;
}

.sg-word-list {
  font-family: "SF Mono", "Fira Code", Menlo, Consolas, monospace;
  font-size: 0.625rem;
  color: var(--text-secondary);
  margin-top: 2px;
  max-width: 200px;
  word-break: break-word;
}

.sg-mark-row {
  display: flex;
  gap: 0.25rem;
  margin-top: 4px;
}

.sg-mark-btn {
  background: var(--bg-tertiary);
  border: 1px solid var(--border);
  color: var(--text-secondary);
  font-size: 0.75rem;
  padding: 0.125rem 0.5rem;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.15s, border-color 0.15s, color 0.15s;
}

.sg-mark-btn:hover {
  border-color: var(--accent);
  color: var(--text-primary);
}

.sg-mark-active-good {
  background: var(--green-emphasis) !important;
  border-color: var(--green) !important;
  color: #fff !important;
}

.sg-mark-active-bad {
  background: var(--red-emphasis) !important;
  border-color: var(--red) !important;
  color: #fff !important;
}
"##;

const HTML_JS: &str = r##"
(function() {
  'use strict';

  const KEY = 'orca-marks-' + location.pathname;
  let marks = new Map();
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) marks = new Map(JSON.parse(raw));
  } catch(e) {}

  function saveMarks() {
    if (marks.size === 0) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, JSON.stringify([...marks]));
  }

  // Parse grid text into flat char array
  function parseChars(text) {
    const lines = text.split('\n').filter(l => l.length > 0);
    const chars = [];
    for (const line of lines) {
      for (const ch of line) chars.push(ch);
    }
    return chars;
  }

  // Count fillable (non-black) cells
  function countFillable(chars) {
    let n = 0;
    for (const ch of chars) if (ch !== '#') n++;
    return n;
  }

  // Group solutions by similarity (10% wildcard tolerance)
  function collapseAll(texts) {
    const groups = [];
    const tolerance = 0.1;

    for (const text of texts) {
      const chars = parseChars(text);
      const fillable = countFillable(chars);
      const maxDiff = Math.max(1, Math.floor(fillable * tolerance));
      let merged = false;

      for (const g of groups) {
        let diff = 0;
        let ok = true;
        for (let i = 0; i < chars.length && i < g.template.length; i++) {
          if (chars[i] !== g.template[i] && g.template[i] !== '?' && chars[i] !== '#') {
            diff++;
            if (diff > maxDiff) { ok = false; break; }
          }
        }
        if (ok && diff <= maxDiff) {
          // Merge into group
          g.count++;
          g.members.push(text);
          for (let i = 0; i < chars.length; i++) {
            if (chars[i] !== g.template[i] && chars[i] !== '#') {
              if (!g.variants[i]) g.variants[i] = new Set();
              g.variants[i].add(chars[i]);
              // Also add the template's original letter as a variant
              if (g.template[i] !== '?') {
                g.variants[i].add(g.template[i]);
              }
              g.template[i] = '?';
            }
          }
          merged = true;
          break;
        }
      }

      if (!merged) {
        groups.push({
          template: chars.slice(),
          count: 1,
          variants: {},
          members: [text],
        });
      }
    }
    return groups;
  }

  // Extract words of length >= minLen from a grid
  function extractWords(chars, rows, cols, minLen) {
    const words = [];
    // Across
    for (let r = 0; r < rows; r++) {
      let word = '';
      for (let c = 0; c <= cols; c++) {
        const ch = c < cols ? chars[r * cols + c] : '#';
        if (ch !== '#' && ch !== '?') {
          word += ch;
        } else {
          if (word.length >= minLen) words.push(word);
          word = '';
        }
      }
    }
    // Down
    for (let c = 0; c < cols; c++) {
      let word = '';
      for (let r = 0; r <= rows; r++) {
        const ch = r < rows ? chars[r * cols + c] : '#';
        if (ch !== '#' && ch !== '?') {
          word += ch;
        } else {
          if (word.length >= minLen) words.push(word);
          word = '';
        }
      }
    }
    return [...new Set(words)];
  }

  // Render a solution grid as HTML
  function renderGrid(chars, rows, cols, variants) {
    let html = '<div class="solution-grid" style="grid-template-columns:repeat(' + cols + ',24px);grid-template-rows:repeat(' + rows + ',24px)">';
    for (let i = 0; i < chars.length; i++) {
      const ch = chars[i];
      if (ch === '#') {
        html += '<div class="sg-cell sg-black"></div>';
      } else if (ch === '?') {
        const v = variants[i];
        const varStr = v ? [...v].sort().join(', ') : '';
        html += '<div class="sg-cell sg-variant" data-variants="' + varStr + '">?</div>';
      } else {
        html += '<div class="sg-cell sg-letter">' + ch + '</div>';
      }
    }
    html += '</div>';
    return html;
  }

  // Get mark for a group (check first member)
  function getGroupMark(g) {
    for (const m of g.members) {
      const v = marks.get(m);
      if (v === 'good') return 'good';
      if (v === 'bad') return 'bad';
    }
    return null;
  }

  // Render everything
  function renderAll(groups) {
    // Sort: good first, unmarked middle, bad last
    groups.sort((a, b) => {
      const ma = getGroupMark(a);
      const mb = getGroupMark(b);
      const tier = m => m === 'good' ? 0 : m === 'bad' ? 2 : 1;
      return tier(ma) - tier(mb);
    });

    // Summary
    let kept = 0, skipped = 0;
    for (const g of groups) {
      const m = getGroupMark(g);
      if (m === 'good') kept++;
      else if (m === 'bad') skipped++;
    }
    const unclassified = groups.length - kept - skipped;
    const totalSolutions = groups.reduce((s, g) => s + g.count, 0);
    document.getElementById('summary').textContent =
      totalSolutions + ' solutions in ' + groups.length + ' groups (' +
      kept + ' kept, ' + skipped + ' skipped, ' + unclassified + ' unclassified)';

    // Groups
    const container = document.getElementById('groups');
    container.innerHTML = '';
    for (let gi = 0; gi < groups.length; gi++) {
      const g = groups[gi];
      const mark = getGroupMark(g);
      const groupDiv = document.createElement('div');
      groupDiv.className = 'sg-group' +
        (mark === 'good' ? ' sg-group-good' : '') +
        (mark === 'bad' ? ' sg-group-bad' : '');
      groupDiv.dataset.groupIdx = gi;

      const gridHtml = renderGrid(g.template, GRID_ROWS, GRID_COLS, g.variants);

      const longWords = extractWords(g.template, GRID_ROWS, GRID_COLS, 10);
      const wordHtml = longWords.length > 0
        ? '<div class="sg-word-list">' + longWords.join(', ') + '</div>'
        : '';

      const badge = g.count > 1 ? '<span class="sg-count-badge">\u00d7' + g.count + '</span>' : '';

      groupDiv.innerHTML = gridHtml +
        '<div class="sg-info">' + badge + '</div>' +
        wordHtml +
        '<div class="sg-mark-row">' +
          '<button class="sg-mark-btn' + (mark === 'good' ? ' sg-mark-active-good' : '') +
            '" data-group="' + gi + '" data-mark="good">\u25b2 Keep</button>' +
          '<button class="sg-mark-btn' + (mark === 'bad' ? ' sg-mark-active-bad' : '') +
            '" data-group="' + gi + '" data-mark="bad">\u25bc Skip</button>' +
        '</div>';

      container.appendChild(groupDiv);
    }
  }

  // Group and render
  const groups = collapseAll(SOLUTIONS);
  renderAll(groups);

  // Mark button delegation
  document.getElementById('groups').addEventListener('click', function(e) {
    const btn = e.target.closest('.sg-mark-btn');
    if (!btn) return;
    const gi = parseInt(btn.dataset.group);
    const action = btn.dataset.mark;
    const g = groups[gi];
    const currentMark = getGroupMark(g);

    // Toggle: clicking same mark again clears it
    const newMark = currentMark === action ? null : action;

    // Apply to all members
    for (const m of g.members) {
      if (newMark) marks.set(m, newMark);
      else marks.delete(m);
    }
    saveMarks();
    renderAll(groups);
  });
})();
"##;
