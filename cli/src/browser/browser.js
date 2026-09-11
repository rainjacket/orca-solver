
(function() {
  'use strict';

  const KEY = 'orca-marks-' + location.pathname;
  const PAGE_SIZE = 100;
  let page = 0;
  const solutionIds = new Map(SOLUTIONS.map((text, id) => [text, id]));
  let marks = new Map();
  function validMark(mark) { return mark === 'good' || mark === 'bad'; }
  function showStorageStatus(text) {
    document.getElementById('storage-status').textContent = text;
  }
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) {
      const saved = JSON.parse(raw);
      const entries = Array.isArray(saved) ? saved : saved.version === 2 ? saved.marks : [];
      for (const [key, mark] of entries) {
        const text = Array.isArray(saved) ? key : SOLUTIONS[key];
        if (solutionIds.has(text) && validMark(mark)) marks.set(text, mark);
      }
    }
  } catch(e) { showStorageStatus('Saved marks could not be loaded. You can import an exported copy.'); }

  function saveMarks() {
    try {
      if (marks.size === 0) localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, JSON.stringify({
        version: 2, marks: [...marks].map(([text, mark]) => [solutionIds.get(text), mark])
      }));
      showStorageStatus('');
    } catch(e) {
      showStorageStatus('Marks are kept for this session, but could not be saved. Export them before closing this page.');
    }
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
    const pages = Math.max(1, Math.ceil(groups.length / PAGE_SIZE));
    page = Math.min(page, pages - 1);
    document.getElementById('page-status').textContent = 'Page ' + (page + 1) + ' of ' + pages;
    document.getElementById('prev-page').disabled = page === 0;
    document.getElementById('next-page').disabled = page === pages - 1;
    for (let gi = page * PAGE_SIZE; gi < Math.min(groups.length, (page + 1) * PAGE_SIZE); gi++) {
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
  document.getElementById('prev-page').addEventListener('click', () => {
    if (page > 0) { page--; renderAll(groups); }
  });
  document.getElementById('next-page').addEventListener('click', () => {
    if ((page + 1) * PAGE_SIZE < groups.length) { page++; renderAll(groups); }
  });
  document.getElementById('export-marks').addEventListener('click', () => {
    // Export by grid text so marks remain portable across differently ordered files.
    const blob = new Blob([JSON.stringify([...marks])], {type: 'application/json'});
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url; link.download = 'orca-marks.json'; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  document.getElementById('import-marks').addEventListener('change', async event => {
    const file = event.target.files[0];
    if (!file) return;
    try {
      const entries = JSON.parse(await file.text());
      if (!Array.isArray(entries) || !entries.every(entry =>
          Array.isArray(entry) && entry.length === 2 && typeof entry[0] === 'string' && validMark(entry[1]))) {
        throw new Error('Invalid marks file');
      }
      for (const [text, mark] of entries) {
        if (solutionIds.has(text)) marks.set(text, mark);
      }
      saveMarks(); renderAll(groups);
    } catch(e) { showStorageStatus('Could not import marks: invalid or unreadable marks file.'); }
    event.target.value = '';
  });

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
