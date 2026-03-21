//! Crossword grid representation: cells, slots, and crossings.
//! Parses `.grid` files and extracts the constraint structure needed by the solver.

use anyhow::{bail, Context, Result};
use std::fmt;
use std::fs;
use std::path::Path;

/// A cell in the crossword grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Black square (blocked).
    Black,
    /// Must be filled with a letter.
    Fill,
    /// Wild cell: ignored by the solver (no dictionary or crossing constraints).
    Wild,
    /// Pre-filled with a specific letter (0-25 = A-Z).
    Letter(u8),
    /// Subset constraint: a 26-bit mask where bit 0 = A, bit 1 = B, ..., bit 25 = Z.
    Subset(u32),
}

impl Cell {
    /// Whether this cell participates in slots (non-black).
    pub fn is_open(&self) -> bool {
        !matches!(self, Cell::Black)
    }

    /// Whether this cell is constrained (must match a dictionary word).
    pub fn is_constrained(&self) -> bool {
        matches!(self, Cell::Fill | Cell::Letter(_) | Cell::Subset(_))
    }
}

/// Direction of a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Across,
    Down,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Across => write!(f, "Across"),
            Direction::Down => write!(f, "Down"),
        }
    }
}

/// A slot is a maximal run of non-black cells in a row or column.
/// A slot's index in `Grid::slots` serves as its unique identifier.
#[derive(Debug, Clone)]
pub struct Slot {
    pub direction: Direction,
    /// (row, col) of the starting cell.
    pub start: (usize, usize),
    /// Length in cells.
    pub len: usize,
    /// Cells composing this slot, as (row, col) coordinates.
    pub cells: Vec<(usize, usize)>,
    /// Whether this slot is constrained (has at least one Fill, Letter, or Subset cell).
    pub constrained: bool,
    /// Check-only slot: constrained but should not be enumerated during search.
    /// True for slots spanning both constrained and wild cells (tendrils into
    /// corners) — we verify their domain is non-empty but don't assign words to them.
    pub check_only: bool,
    /// Pre-filled pattern: `Some(mask)` for constrained cells, `None` for unknowns.
    /// Single letter: `Some(1 << letter_index)`. Subset: `Some(mask)` with multiple bits.
    /// WILD cells are None here (unconstrained).
    pub pattern: Vec<Option<u32>>,
}

/// A crossing between two slots at shared cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crossing {
    pub slot_a: usize,
    pub pos_in_a: usize,
    pub slot_b: usize,
    pub pos_in_b: usize,
}

/// Whether a line is a grid comment (before the dimensions line).
/// Comments are lines starting with "# " or exactly "#".
fn is_comment_line(line: &str) -> bool {
    line.starts_with("# ") || line == "#"
}

/// Parse a single grid row, handling bracket notation [AEIOU] for subset constraints.
fn parse_grid_row(line: &str) -> Result<Vec<Cell>> {
    let mut cells = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch == '[' {
            chars.next(); // consume '['
            let mut mask = 0u32;
            let mut count = 0u32;
            loop {
                match chars.next() {
                    Some(']') => break,
                    Some(c) if c.is_ascii_uppercase() => {
                        mask |= 1u32 << (c as u8 - b'A');
                        count += 1;
                    }
                    Some(c) => bail!("Invalid character '{}' in bracket notation", c),
                    None => bail!("Unclosed bracket notation"),
                }
            }
            if count == 0 {
                bail!("Empty bracket notation");
            } else if count == 1 {
                // Single letter: normalize to Cell::Letter
                let letter = mask.trailing_zeros() as u8;
                cells.push(Cell::Letter(letter));
            } else if count == 26 {
                // All letters: normalize to Cell::Fill
                cells.push(Cell::Fill);
            } else {
                cells.push(Cell::Subset(mask));
            }
        } else {
            chars.next();
            cells.push(match ch {
                '#' => Cell::Black,
                '.' => Cell::Fill,
                '*' => Cell::Wild,
                'A'..='Z' => Cell::Letter(ch as u8 - b'A'),
                _ => bail!("Invalid grid character: '{}'", ch),
            });
        }
    }
    Ok(cells)
}

/// Returns byte ranges for each cell token in a grid data line.
/// Single chars map to 1-byte ranges; `[...]` bracket tokens map to the full bracket range.
pub fn cell_ranges(line: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b']' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // consume ']'
            }
            ranges.push((start, i));
        } else {
            ranges.push((i, i + 1));
            i += 1;
        }
    }
    ranges
}

/// The parsed crossword grid.
#[derive(Debug, Clone)]
pub struct Grid {
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<Vec<Cell>>,
    pub slots: Vec<Slot>,
    pub crossings: Vec<Crossing>,
}

impl Grid {
    /// Load and parse a `.grid` file.
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read grid file: {}", path.display()))?;
        Self::parse(&content)
    }

    /// Parse grid content from a string.
    pub fn parse(content: &str) -> Result<Self> {
        // Only strip comments before the dimensions line. Once we have dimensions,
        // all non-empty lines are grid data (# is also the black square character).
        let mut all_lines = content.lines().map(|l| l.trim()).filter(|l| !l.is_empty());

        // Skip comment lines to find the dimensions line
        let dims_line = loop {
            match all_lines.next() {
                Some(line) if is_comment_line(line) => continue,
                Some(line) => break line,
                None => bail!("Empty grid file"),
            }
        };

        let dims: Vec<usize> = dims_line
            .split_whitespace()
            .map(|s| s.parse::<usize>())
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to parse grid dimensions")?;

        if dims.len() != 2 {
            bail!("Expected 2 dimensions (rows cols), got {}", dims.len());
        }

        let rows = dims[0];
        let cols = dims[1];

        // Collect remaining non-empty lines as grid rows (no comment stripping)
        let lines: Vec<&str> = all_lines.collect();

        if lines.len() != rows {
            bail!("Expected {} rows of grid data, got {}", rows, lines.len());
        }

        // Parse cells (supports bracket notation: [AEIOU] for subset constraints)
        let mut cells = Vec::with_capacity(rows);
        for (r, line) in lines.iter().enumerate() {
            let row = parse_grid_row(line).with_context(|| format!("Row {}", r))?;
            if row.len() != cols {
                bail!("Row {} has {} columns, expected {}", r, row.len(), cols);
            }
            cells.push(row);
        }

        // Extract slots (across then down)
        let mut slots = Vec::new();

        /// Scan one direction for slots: `outer_len` and `inner_len` are the
        /// major/minor dimensions, `to_rc` maps (outer, inner) → (row, col).
        fn extract_slots(
            cells: &[Vec<Cell>],
            dir: Direction,
            outer_len: usize,
            inner_len: usize,
            to_rc: fn(usize, usize) -> (usize, usize),
            slots: &mut Vec<Slot>,
        ) {
            for outer in 0..outer_len {
                let mut inner = 0;
                while inner < inner_len {
                    let (r, c) = to_rc(outer, inner);
                    if cells[r][c].is_open() {
                        let start = inner;
                        while inner < inner_len {
                            let (r2, c2) = to_rc(outer, inner);
                            if !cells[r2][c2].is_open() {
                                break;
                            }
                            inner += 1;
                        }
                        let len = inner - start;
                        if len >= 3 {
                            let slot_cells: Vec<(usize, usize)> =
                                (start..inner).map(|i| to_rc(outer, i)).collect();
                            let constrained = slot_cells
                                .iter()
                                .any(|&(r, c)| cells[r][c].is_constrained());
                            let has_wild =
                                slot_cells.iter().any(|&(r, c)| cells[r][c] == Cell::Wild);
                            let pattern: Vec<Option<u32>> = slot_cells
                                .iter()
                                .map(|&(r, c)| match cells[r][c] {
                                    Cell::Letter(l) => Some(1u32 << l),
                                    Cell::Subset(mask) => Some(mask),
                                    _ => None,
                                })
                                .collect();
                            let (sr, sc) = to_rc(outer, start);
                            slots.push(Slot {
                                direction: dir,
                                start: (sr, sc),
                                len,
                                cells: slot_cells,
                                constrained,
                                check_only: constrained && has_wild,
                                pattern,
                            });
                        }
                    } else {
                        inner += 1;
                    }
                }
            }
        }

        extract_slots(
            &cells,
            Direction::Across,
            rows,
            cols,
            |r, c| (r, c),
            &mut slots,
        );
        extract_slots(
            &cells,
            Direction::Down,
            cols,
            rows,
            |c, r| (r, c),
            &mut slots,
        );

        // Compute crossings: for each cell shared by two slots, record crossing.
        // Skip crossings at WILD cells (unconstrained).
        // Use BTreeMap for deterministic crossing order (HashMap iteration is random).
        let mut cell_to_slot: std::collections::BTreeMap<(usize, usize), Vec<(usize, usize)>> =
            std::collections::BTreeMap::new();
        for (slot_idx, slot) in slots.iter().enumerate() {
            for (pos, &cell_coord) in slot.cells.iter().enumerate() {
                cell_to_slot
                    .entry(cell_coord)
                    .or_default()
                    .push((slot_idx, pos));
            }
        }

        let mut crossings = Vec::new();
        for (&(r, c), slot_refs) in &cell_to_slot {
            if slot_refs.len() == 2 {
                // Skip crossings at WILD cells
                if cells[r][c] == Cell::Wild {
                    continue;
                }
                let (slot_a, pos_a) = slot_refs[0];
                let (slot_b, pos_b) = slot_refs[1];
                crossings.push(Crossing {
                    slot_a,
                    pos_in_a: pos_a,
                    slot_b,
                    pos_in_b: pos_b,
                });
            }
        }

        Ok(Grid {
            rows,
            cols,
            cells,
            slots,
            crossings,
        })
    }

    /// Format the grid with assigned words into a display string.
    pub fn format_filled(&self, assignments: &[Option<&str>]) -> String {
        let mut display = vec![vec!['#'; self.cols]; self.rows];

        // First, mark all open cells
        for (r, display_row) in display.iter_mut().enumerate() {
            for (c, cell) in display_row.iter_mut().enumerate() {
                match self.cells[r][c] {
                    Cell::Black => *cell = '#',
                    Cell::Wild => *cell = '*',
                    Cell::Fill | Cell::Subset(_) => *cell = '.',
                    Cell::Letter(l) => *cell = (l + b'A') as char,
                }
            }
        }

        // Fill in assigned words
        for (slot_id, slot) in self.slots.iter().enumerate() {
            if let Some(Some(word)) = assignments.get(slot_id) {
                for (pos, &(r, c)) in slot.cells.iter().enumerate() {
                    if let Some(ch) = word.as_bytes().get(pos) {
                        display[r][c] = *ch as char;
                    }
                }
            }
        }

        display
            .iter()
            .map(|row| row.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Check whether the grid has diagonal symmetry: rows == cols and
    /// for all (r,c), cell[r][c] and cell[c][r] are compatible across
    /// the diagonal (same black/open type, and any pre-filled letters match).
    pub fn has_diagonal_symmetry(&self) -> bool {
        if self.rows != self.cols {
            return false;
        }
        for r in 0..self.rows {
            for c in (r + 1)..self.cols {
                if !cells_diagonal_compatible(self.cells[r][c], self.cells[c][r]) {
                    return false;
                }
            }
        }
        true
    }
}

/// Two cells are diagonal-symmetry compatible: both black, or both non-black
/// with matching seeds (pre-filled letters must be equal across the diagonal).
fn cells_diagonal_compatible(a: Cell, b: Cell) -> bool {
    match (a, b) {
        (Cell::Black, Cell::Black) => true,
        (Cell::Black, _) | (_, Cell::Black) => false,
        (Cell::Letter(x), Cell::Letter(y)) => x == y,
        (Cell::Letter(_), _) | (_, Cell::Letter(_)) => false,
        _ => true,
    }
}

/// Skip comment/empty lines and the dimensions line, returning only grid data rows.
pub fn grid_data_lines(text: &str) -> Vec<&str> {
    let mut lines = text.lines();
    loop {
        match lines.next() {
            Some(l) if is_comment_line(l.trim()) || l.trim().is_empty() => continue,
            Some(_) => break, // skip dimensions line
            None => break,
        }
    }
    lines.collect()
}

/// Find the index of the first data line (after comments and dimension header).
fn find_data_start(lines: &[String]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_comment_line(trimmed) {
            continue;
        }
        // This is the dimensions line — data starts after it
        return i + 1;
    }
    0
}

/// Replace the cell at logical column `col` in a grid data line with `replacement`.
/// Handles both single-char cells and `[...]` bracket tokens.
fn replace_cell_in_line(line: &str, col: usize, replacement: &str) -> String {
    let ranges = cell_ranges(line);
    if col >= ranges.len() {
        return line.to_string();
    }
    let (start, end) = ranges[col];
    let mut result = String::with_capacity(line.len());
    result.push_str(&line[..start]);
    result.push_str(replacement);
    result.push_str(&line[end..]);
    result
}

/// Set a single letter at a grid cell position in raw grid text.
///
/// Used for letter-level seeding during partition sub-splitting.
/// Handles variable-width cell tokens (bracket notation).
pub fn set_letter_in_grid_text(grid_text: &str, row: usize, col: usize, letter: char) -> String {
    set_cell_in_grid_text(grid_text, row, col, &letter.to_string())
}

/// Replace a cell in raw grid text with an arbitrary token (letter, bracket subset, etc.).
///
/// Handles variable-width cell tokens (bracket notation).
pub fn set_cell_in_grid_text(grid_text: &str, row: usize, col: usize, token: &str) -> String {
    let mut lines: Vec<String> = grid_text.lines().map(|l| l.to_string()).collect();
    let data_start = find_data_start(&lines);

    let line_idx = data_start + row;
    if line_idx < lines.len() {
        lines[line_idx] = replace_cell_in_line(&lines[line_idx], col, token);
    }

    lines.join("\n")
}

impl fmt::Display for Grid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for row in &self.cells {
            for cell in row {
                match cell {
                    Cell::Black => write!(f, "#")?,
                    Cell::Fill => write!(f, ".")?,
                    Cell::Wild => write!(f, "*")?,
                    Cell::Letter(l) => write!(f, "{}", (l + b'A') as char)?,
                    Cell::Subset(mask) => {
                        write!(f, "[")?;
                        for i in 0..26u8 {
                            if mask & (1 << i) != 0 {
                                write!(f, "{}", (i + b'A') as char)?;
                            }
                        }
                        write!(f, "]")?;
                    }
                }
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fill letters at slot cell positions in raw grid text (test helper).
    fn fill_word_in_grid_text(grid_text: &str, cells: &[(usize, usize)], word: &str) -> String {
        let mut lines: Vec<String> = grid_text.lines().map(|l| l.to_string()).collect();
        let data_start = find_data_start(&lines);

        let word_bytes = word.as_bytes();
        for (pos, &(row, col)) in cells.iter().enumerate() {
            if pos < word_bytes.len() {
                let line_idx = data_start + row;
                if line_idx < lines.len() {
                    let ch = word_bytes[pos] as char;
                    lines[line_idx] = replace_cell_in_line(&lines[line_idx], col, &ch.to_string());
                }
            }
        }

        lines.join("\n")
    }

    #[test]
    fn test_parse_simple() {
        let grid_str = "5 5\n.....\n.....\n.....\n.....\n.....\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert_eq!(grid.rows, 5);
        assert_eq!(grid.cols, 5);
        // 5 across + 5 down = 10 slots
        assert_eq!(grid.slots.len(), 10);
    }

    #[test]
    fn test_parse_with_blacks() {
        let grid_str = "5 5\n..#..\n.....\n#...#\n.....\n..#..\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert_eq!(grid.rows, 5);
        assert_eq!(grid.cols, 5);
        // 3 across (rows 1,2,3) + 3 down (cols 1,2,3) = 6 slots
        // Rows 0,4 and cols 0,4 have only 2-cell runs (split by blacks)
        assert_eq!(grid.slots.len(), 6);
        for slot in &grid.slots {
            assert!(slot.len >= 3);
        }
    }

    #[test]
    fn test_slot_extraction() {
        // A simple 3x3 grid with no blacks: should have 2 slots (1 across, 1 down... no, 3 across + 3 down)
        // Actually: 3 rows of 3 = 3 across slots, 3 cols of 3 = 3 down slots
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert_eq!(grid.slots.len(), 6); // 3 across + 3 down
        for slot in &grid.slots {
            assert_eq!(slot.len, 3);
        }
    }

    #[test]
    fn test_slot_too_short() {
        // 2-letter runs shouldn't become slots
        let grid_str = "3 3\n..#\n..#\n###\n";
        let grid = Grid::parse(grid_str).unwrap();
        // Only 2-cell runs, no slots of length >= 3
        assert_eq!(grid.slots.len(), 0);
    }

    #[test]
    fn test_crossings() {
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        // 3x3 grid: 9 cells, each shared by 1 across and 1 down slot = 9 crossings
        assert_eq!(grid.crossings.len(), 9);
    }

    #[test]
    fn test_wild_cells() {
        let grid_str = "3 5\n*...*\n.....\n*...*\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert_eq!(grid.cells[0][0], Cell::Wild);
        assert_eq!(grid.cells[0][4], Cell::Wild);
        // Wild cell crossings should be omitted
        for crossing in &grid.crossings {
            let (r, c) = grid.slots[crossing.slot_a].cells[crossing.pos_in_a];
            assert_ne!(grid.cells[r][c], Cell::Wild);
        }
    }

    #[test]
    fn test_prefilled_letters() {
        let grid_str = "3 3\nA..\n...\n..C\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert_eq!(grid.cells[0][0], Cell::Letter(0)); // A
        assert_eq!(grid.cells[2][2], Cell::Letter(2)); // C
    }

    #[test]
    fn test_constrained_slot() {
        let grid_str = "1 5\n*...*\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert_eq!(grid.slots.len(), 1);
        assert!(grid.slots[0].constrained); // has FILL cells
    }

    #[test]
    fn test_all_wild_slot() {
        let grid_str = "1 5\n*****\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert_eq!(grid.slots.len(), 1);
        assert!(!grid.slots[0].constrained); // all wild = unconstrained
    }

    #[test]
    fn test_comments_ignored() {
        // Comments before dimensions are stripped
        let grid_str = "# This is a comment\n# Another comment\n3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert_eq!(grid.rows, 3);
        assert_eq!(grid.cols, 3);
    }

    #[test]
    fn test_fill_word_in_grid_text() {
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        // Fill the first across slot (row 0, cols 0-2)
        let cells = &grid.slots[0].cells;
        let filled = fill_word_in_grid_text(grid_str, cells, "CAT");
        // Should round-trip: the filled grid should parse and have 'C','A','T' in row 0
        let parsed = Grid::parse(&filled).unwrap();
        assert_eq!(parsed.cells[0][0], Cell::Letter(2)); // C
        assert_eq!(parsed.cells[0][1], Cell::Letter(0)); // A
        assert_eq!(parsed.cells[0][2], Cell::Letter(19)); // T
                                                          // Other rows unchanged
        assert_eq!(parsed.cells[1][0], Cell::Fill);
    }

    #[test]
    fn test_fill_word_in_grid_text_with_comments() {
        let grid_str = "# A comment\n3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        let cells = &grid.slots[0].cells;
        let filled = fill_word_in_grid_text(grid_str, cells, "DOG");
        let parsed = Grid::parse(&filled).unwrap();
        assert_eq!(parsed.cells[0][0], Cell::Letter(3)); // D
        assert_eq!(parsed.cells[0][1], Cell::Letter(14)); // O
        assert_eq!(parsed.cells[0][2], Cell::Letter(6)); // G
    }

    #[test]
    fn test_set_letter_in_grid_text() {
        let grid_str = "3 3\n...\n...\n...\n";
        let filled = super::set_letter_in_grid_text(grid_str, 1, 1, 'X');
        let parsed = Grid::parse(&filled).unwrap();
        assert_eq!(parsed.cells[1][1], Cell::Letter(23)); // X
        assert_eq!(parsed.cells[0][0], Cell::Fill); // unchanged
    }

    #[test]
    fn test_format_filled() {
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        // Assign words to across slots (first 3 slots should be across)
        let mut assignments: Vec<Option<&str>> = vec![None; grid.slots.len()];
        assignments[0] = Some("CAT");
        assignments[1] = Some("DOG");
        assignments[2] = Some("RAT");
        let filled = grid.format_filled(&assignments);
        let lines: Vec<&str> = filled.lines().collect();
        assert_eq!(lines[0], "CAT");
        assert_eq!(lines[1], "DOG");
        assert_eq!(lines[2], "RAT");
    }

    #[test]
    fn test_diagonal_symmetry_square() {
        // Black cells mirror across diagonal: (0,2) and (2,0)
        let grid_str = "3 3\n..#\n...\n#..\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert!(grid.has_diagonal_symmetry());
    }

    #[test]
    fn test_diagonal_symmetry_all_open() {
        let grid_str = "3 3\n...\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert!(grid.has_diagonal_symmetry());
    }

    #[test]
    fn test_diagonal_symmetry_asymmetric() {
        // (0,2) is black but (2,0) is open
        let grid_str = "3 3\n..#\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert!(!grid.has_diagonal_symmetry());
    }

    #[test]
    fn test_diagonal_symmetry_nonsquare() {
        let grid_str = "3 4\n....\n....\n....\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert!(!grid.has_diagonal_symmetry());
    }

    #[test]
    fn test_diagonal_symmetry_matching_seeds() {
        // Same letter at (0,1) and (1,0) — symmetric
        let grid_str = "3 3\n.A.\nA..\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert!(grid.has_diagonal_symmetry());
    }

    #[test]
    fn test_diagonal_symmetry_mismatched_seeds() {
        // A at (0,1) but B at (1,0) — breaks symmetry
        let grid_str = "3 3\n.A.\nB..\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert!(!grid.has_diagonal_symmetry());
    }

    #[test]
    fn test_diagonal_symmetry_seed_vs_open() {
        // A at (0,2) but open at (2,0) — breaks symmetry
        let grid_str = "3 3\n..A\n...\n...\n";
        let grid = Grid::parse(grid_str).unwrap();
        assert!(!grid.has_diagonal_symmetry());
    }
}
