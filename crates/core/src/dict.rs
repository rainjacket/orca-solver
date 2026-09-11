//! Dictionary loading and indexing. Words are grouped by length into
//! [`LengthBucket`]s with precomputed `letter_bits[position][letter]` bitset
//! indexes for O(1) candidate filtering during constraint propagation.

use crate::bitset::BitSet;
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};

// Packed subset order matches the extraction in letter_union. Q reuses its index.
const UNION_GROUPS: [&[usize]; 6] = [
    &[0, 4, 7, 8, 14, 20],    // AEHIOU
    &[1, 2, 6, 12, 15],       // BCGMP
    &[3, 11, 13, 17, 18, 19], // DLNRST
    &[5, 10, 21, 22, 24],     // FKVWY
    &[9, 23, 25],             // JXZ
    &[16],                    // Q (existing index)
];
struct UnionLayout {
    letter_group: [usize; 26],
    group_masks: [u32; 6],
    offsets: [usize; 6],
    rows: usize,
}

// Derive lookup metadata at compile time; only subset packing in the hot loop
// is specialized. Keep the group definition above as the source of truth.
const fn union_layout() -> UnionLayout {
    let mut layout = UnionLayout {
        letter_group: [0; 26],
        group_masks: [0; 6],
        offsets: [0; 6],
        rows: 0,
    };
    let mut group = 0;
    while group < UNION_GROUPS.len() {
        let letters = UNION_GROUPS[group];
        layout.offsets[group] = layout.rows;
        if letters.len() > 1 {
            layout.rows += 1 << letters.len();
        }
        let mut bit = 0;
        while bit < letters.len() {
            let letter = letters[bit];
            layout.letter_group[letter] = group;
            layout.group_masks[group] |= 1 << letter;
            bit += 1;
        }
        group += 1;
    }
    layout
}
const UNION_LAYOUT: UnionLayout = union_layout();

/// All words of a given length, with precomputed bitset indexes for fast filtering.
#[derive(Debug, Clone)]
pub struct LengthBucket {
    /// Immutable letter-group union tables, allocated only at positions used by filtering.
    /// Clones and concurrent searches share each position's one-time initialization.
    unions: Arc<[OnceLock<Box<[u64]>>]>,
    /// All words of this length, indexed by word_id.
    pub words: Vec<String>,
    /// `letter_bits[position][letter]` -> bitset of word_ids that have `letter` at `position`.
    /// Position is 0-indexed, letter is 0-25 (A-Z).
    pub letter_bits: Vec<[BitSet; 26]>,
    /// All-ones bitset (starting domain for a slot of this length).
    pub all: BitSet,
    /// Flat byte array: `word_bytes[word_id * word_len() + pos]` = letter index (0-25).
    /// Eliminates String pointer chases in small-domain iteration paths.
    pub word_bytes: Vec<u8>,
}

impl LengthBucket {
    /// Word length for this bucket.
    pub fn word_len(&self) -> usize {
        self.letter_bits.len()
    }

    /// Materialize the words whose letter at `pos` belongs to `allowed`.
    pub fn letter_union(&self, pos: usize, allowed: u32, out: &mut [u64]) {
        debug_assert_eq!(out.len(), self.all.blocks().len());
        let mut sources = [&[][..]; 6];
        let count = self.letter_sources(pos, allowed, &mut sources);
        if count == 0 {
            out.fill(0);
            return;
        }
        out.copy_from_slice(sources[0]);
        for source in &sources[1..count] {
            for (dst, src) in out.iter_mut().zip(*source) {
                *dst |= src;
            }
        }
    }

    /// Intersect a domain with the allowed-letter union and count removals.
    /// Select sources once, then combine and intersect each block without
    /// materializing a temporary filter. The caller preserves its snapshot.
    #[inline]
    pub fn intersect_letter_union(
        &self,
        pos: usize,
        allowed: u32,
        domain: &mut crate::domain::CandidateSet,
    ) -> u32 {
        debug_assert_eq!(domain.bits().blocks().len(), self.all.blocks().len());
        let mut sources = [&[][..]; 6];
        let count = self.letter_sources(pos, allowed, &mut sources);
        domain.intersect_union(&sources, count)
    }

    /// Select at most one precomputed subset per group. Zero and singleton
    /// masks bypass lazy table initialization; Q uses its existing index.
    #[inline]
    fn letter_sources<'a>(
        &'a self,
        pos: usize,
        allowed: u32,
        sources: &mut [&'a [u64]; 6],
    ) -> usize {
        debug_assert_eq!(allowed >> 26, 0);
        if allowed == 0 {
            return 0;
        }
        if allowed.is_power_of_two() {
            sources[0] = self.letter_bits[pos][allowed.trailing_zeros() as usize].blocks();
            return 1;
        }
        let n = self.all.blocks().len();
        let table = self.unions[pos].get_or_init(|| {
            let mut table = vec![0; UNION_LAYOUT.rows * n];
            for (group, letters) in UNION_GROUPS.iter().enumerate() {
                if letters.len() == 1 {
                    continue;
                }
                for subset in 1usize..(1 << letters.len()) {
                    let previous = subset & (subset - 1);
                    let letter = letters[subset.trailing_zeros() as usize];
                    let single = self.letter_bits[pos][letter].blocks();
                    let dst = (UNION_LAYOUT.offsets[group] + subset) * n;
                    let src = (UNION_LAYOUT.offsets[group] + previous) * n;
                    for b in 0..n {
                        table[dst + b] = table[src + b] | single[b];
                    }
                }
            }
            table.into_boxed_slice()
        });
        let mut remaining = allowed;
        let mut count = 0;
        while remaining != 0 {
            let group = UNION_LAYOUT.letter_group[remaining.trailing_zeros() as usize];
            let subset = match group {
                0 => {
                    ((remaining & 1)
                        | (((remaining >> 4) & 1) << 1)
                        | (((remaining >> 7) & 3) << 2)
                        | (((remaining >> 14) & 1) << 4)
                        | (((remaining >> 20) & 1) << 5)) as usize
                }
                1 => {
                    (((remaining >> 1) & 3)
                        | (((remaining >> 6) & 1) << 2)
                        | (((remaining >> 12) & 1) << 3)
                        | (((remaining >> 15) & 1) << 4)) as usize
                }
                2 => {
                    (((remaining >> 3) & 1)
                        | (((remaining >> 11) & 1) << 1)
                        | (((remaining >> 13) & 1) << 2)
                        | (((remaining >> 17) & 7) << 3)) as usize
                }
                3 => {
                    (((remaining >> 5) & 1)
                        | (((remaining >> 10) & 1) << 1)
                        | (((remaining >> 21) & 3) << 2)
                        | (((remaining >> 24) & 1) << 4)) as usize
                }
                4 => {
                    (((remaining >> 9) & 1)
                        | (((remaining >> 23) & 1) << 1)
                        | (((remaining >> 25) & 1) << 2)) as usize
                }
                5 => ((remaining >> 16) & 1) as usize,
                _ => unreachable!(),
            };
            remaining &= !UNION_LAYOUT.group_masks[group];
            let source = match group {
                5 => self.letter_bits[pos][UNION_GROUPS[5][0]].blocks(),
                _ => {
                    let start = UNION_LAYOUT.offsets[group] * n + subset * n;
                    &table[start..start + n]
                }
            };
            sources[count] = source;
            count += 1;
        }
        count
    }

    /// Get the set of candidate word_ids matching a partial pattern.
    /// `pattern[i] = Some(mask)` for constrained positions (26-bit letter mask), `None` for unknowns.
    /// Single-bit mask = exact letter. Multi-bit mask = subset (OR of matching letter bitsets).
    /// Returns None if no candidates match (empty intersection).
    pub fn candidates(&self, pattern: &[Option<u32>]) -> Option<BitSet> {
        debug_assert_eq!(pattern.len(), self.letter_bits.len());
        let mut result = self.all.clone();
        for (pos, &mask_opt) in pattern.iter().enumerate() {
            if let Some(mask) = mask_opt {
                if mask.count_ones() == 1 {
                    // Single letter — fast path
                    let letter = mask.trailing_zeros() as usize;
                    if !result.and_with(&self.letter_bits[pos][letter]) {
                        return None;
                    }
                } else {
                    // Subset: OR together bitsets for each allowed letter
                    let num_words = self.all.num_bits();
                    let mut union = BitSet::new(num_words);
                    for letter in 0..26usize {
                        if mask & (1u32 << letter) != 0 {
                            union.or_with(&self.letter_bits[pos][letter]);
                        }
                    }
                    if !result.and_with(&union) {
                        return None;
                    }
                }
            }
        }
        Some(result)
    }
}

/// The complete dictionary, organized by word length.
#[derive(Debug, Clone)]
pub struct Dictionary {
    /// Buckets indexed by word length. `buckets[len]` is `Some(bucket)` if
    /// the dictionary has words of that length, `None` otherwise.
    /// Uses Vec for O(1) lookup (word lengths are small integers, typically 3-21).
    buckets: Vec<Option<LengthBucket>>,
}

impl Dictionary {
    /// Load a dictionary from a `.dict` file (format: `WORD;SCORE\n`).
    /// Words are uppercased, deduplicated, and grouped by length.
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read dictionary file: {}", path.display()))?;
        Self::parse(&content)
    }

    /// Parse dictionary content from a string.
    ///
    /// Lines are `WORD;SCORE`; `#` comment lines are allowed. Words are
    /// uppercased; entries containing non-letters or shorter than 3 letters
    /// are silently skipped. A missing or malformed score is an error.
    pub fn parse(content: &str) -> Result<Self> {
        // First pass: collect unique words (scores parsed for format validation only)
        let mut words: HashSet<String> = HashSet::new();

        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let word_str = match line.split_once(';') {
                Some((w, s)) => {
                    // Validate score format
                    s.trim().parse::<u32>().with_context(|| {
                        format!("Invalid score on line {}: {:?}", line_num + 1, s)
                    })?;
                    w.trim()
                }
                None => bail!(
                    "Invalid dictionary line {} (expected WORD;SCORE): {:?}",
                    line_num + 1,
                    line
                ),
            };

            let word = word_str.to_uppercase();

            // Validate: only A-Z
            if !word.bytes().all(|b| b.is_ascii_uppercase()) {
                continue; // Skip words with non-letter characters (digits, hyphens, etc.)
            }

            // Skip words shorter than 3 letters (not valid crossword entries)
            if word.len() < 3 {
                continue;
            }

            words.insert(word);
        }

        // Group by length
        let mut by_length: HashMap<usize, Vec<String>> = HashMap::new();
        for word in words {
            let len = word.len();
            by_length.entry(len).or_default().push(word);
        }

        // Sort each length group alphabetically (deterministic, no score bias)
        for entries in by_length.values_mut() {
            entries.sort();
        }

        // Build buckets (Vec indexed by word length)
        let max_len = by_length.keys().copied().max().unwrap_or(0);
        let mut buckets: Vec<Option<LengthBucket>> = (0..=max_len).map(|_| None).collect();
        for (len, words) in by_length {
            let num_words = words.len();
            let all = BitSet::new_all_set(num_words);

            // Build letter_bits index
            let mut letter_bits: Vec<[BitSet; 26]> = Vec::with_capacity(len);
            for _ in 0..len {
                letter_bits.push(std::array::from_fn(|_| BitSet::new(num_words)));
            }

            // Build flat word_bytes array and letter_bits index in one pass
            let mut word_bytes = vec![0u8; num_words * len];
            for (word_id, word) in words.iter().enumerate() {
                for (pos, ch) in word.bytes().enumerate() {
                    let letter = (ch - b'A') as usize;
                    letter_bits[pos][letter].set(word_id);
                    word_bytes[word_id * len + pos] = letter as u8;
                }
            }

            buckets[len] = Some(LengthBucket {
                unions: (0..len).map(|_| OnceLock::new()).collect(),
                words,
                letter_bits,
                all,
                word_bytes,
            });
        }

        Ok(Dictionary { buckets })
    }

    /// Get the bucket for a given word length, if any.
    #[inline]
    pub fn bucket(&self, len: usize) -> Option<&LengthBucket> {
        self.buckets.get(len)?.as_ref()
    }

    /// Get all available word lengths.
    pub fn lengths(&self) -> Vec<usize> {
        self.buckets
            .iter()
            .enumerate()
            .filter_map(|(i, b)| if b.is_some() { Some(i) } else { None })
            .collect()
    }

    /// Total number of unique words in the dictionary.
    pub fn total_words(&self) -> usize {
        self.buckets
            .iter()
            .filter_map(|b| b.as_ref())
            .map(|b| b.words.len())
            .sum()
    }

    /// Validate dictionary format, returning a list of issues.
    pub fn validate(path: &Path) -> Result<Vec<String>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read dictionary file: {}", path.display()))?;
        let mut issues = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line.split_once(';') {
                Some((word, score)) => {
                    if score.trim().parse::<u32>().is_err() {
                        issues.push(format!("Line {}: invalid score {:?}", line_num + 1, score));
                    }
                    let word = word.trim().to_uppercase();
                    if !word.bytes().all(|b| b.is_ascii_uppercase()) {
                        issues.push(format!(
                            "Line {}: word contains non-letter characters: {:?}",
                            line_num + 1,
                            word
                        ));
                    } else if word.len() < 3 {
                        issues.push(format!(
                            "Line {}: word shorter than 3 letters (will be skipped): {:?}",
                            line_num + 1,
                            word
                        ));
                    }
                }
                None => {
                    issues.push(format!(
                        "Line {}: missing semicolon separator",
                        line_num + 1
                    ));
                }
            }
        }

        Ok(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dict() -> &'static str {
        "CAT;50\nDOG;40\nCAR;45\nRAT;30\nTAR;35\nARC;25\nFISH;60\nDISH;55\nWISH;50\nHELLO;70\nWORLD;65\n"
    }

    #[test]
    fn test_load_basic() {
        let dict = Dictionary::parse(test_dict()).unwrap();
        assert!(dict.bucket(3).is_some());
        assert!(dict.bucket(4).is_some());
        assert!(dict.bucket(5).is_some());
        assert!(dict.bucket(2).is_none());
    }

    #[test]
    fn test_word_count() {
        let dict = Dictionary::parse(test_dict()).unwrap();
        assert_eq!(dict.bucket(3).unwrap().words.len(), 6); // CAT, DOG, CAR, RAT, TAR, ARC
        assert_eq!(dict.bucket(4).unwrap().words.len(), 3); // FISH, DISH, WISH
        assert_eq!(dict.bucket(5).unwrap().words.len(), 2); // HELLO, WORLD
    }

    /// Helper to create a single-letter mask from a letter byte.
    fn letter_mask(letter: u8) -> u32 {
        1u32 << (letter - b'A')
    }

    #[test]
    fn test_candidates_no_constraint() {
        let dict = Dictionary::parse(test_dict()).unwrap();
        let bucket = dict.bucket(3).unwrap();
        let pattern = vec![None, None, None];
        let cands = bucket.candidates(&pattern).unwrap();
        assert_eq!(cands.count_ones(), 6);
    }

    #[test]
    fn test_candidates_first_letter() {
        let dict = Dictionary::parse(test_dict()).unwrap();
        let bucket = dict.bucket(3).unwrap();
        // Words starting with C: CAT, CAR
        let pattern = vec![Some(letter_mask(b'C')), None, None];
        let cands = bucket.candidates(&pattern).unwrap();
        assert_eq!(cands.count_ones(), 2);
    }

    #[test]
    fn test_candidates_no_match() {
        let dict = Dictionary::parse(test_dict()).unwrap();
        let bucket = dict.bucket(3).unwrap();
        // No 3-letter word starts with Z
        let pattern = vec![Some(letter_mask(b'Z')), None, None];
        let result = bucket.candidates(&pattern);
        assert!(result.is_none());
    }

    #[test]
    fn test_candidates_full_pattern() {
        let dict = Dictionary::parse(test_dict()).unwrap();
        let bucket = dict.bucket(3).unwrap();
        // C_T -> CAT
        let pattern = vec![Some(letter_mask(b'C')), None, Some(letter_mask(b'T'))];
        let cands = bucket.candidates(&pattern).unwrap();
        assert_eq!(cands.count_ones(), 1);
    }

    #[test]
    fn test_candidates_subset_mask() {
        let dict = Dictionary::parse(test_dict()).unwrap();
        let bucket = dict.bucket(3).unwrap();
        // First letter C or D: CAT, CAR, DOG
        let mask = letter_mask(b'C') | letter_mask(b'D');
        let pattern = vec![Some(mask), None, None];
        let cands = bucket.candidates(&pattern).unwrap();
        assert_eq!(cands.count_ones(), 3);
    }

    #[test]
    fn test_dedup() {
        let content = "CAT;50\ncat;30\nCat;40\n";
        let dict = Dictionary::parse(content).unwrap();
        let bucket = dict.bucket(3).unwrap();
        assert_eq!(bucket.words.len(), 1);
        assert_eq!(bucket.words[0], "CAT");
    }

    #[test]
    fn test_sorted_alphabetically() {
        let dict = Dictionary::parse(test_dict()).unwrap();
        let bucket = dict.bucket(3).unwrap();
        // Should be sorted alphabetically
        for window in bucket.words.windows(2) {
            assert!(
                window[0] <= window[1],
                "{} should be before {}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn test_skips_short_words() {
        let content = "AB;50\nA;30\nCAT;40\n";
        let dict = Dictionary::parse(content).unwrap();
        assert!(dict.bucket(1).is_none());
        assert!(dict.bucket(2).is_none());
        assert_eq!(dict.total_words(), 1);
    }

    #[test]
    fn test_parse_rejects_malformed_lines() {
        assert!(Dictionary::parse("CAT\n").is_err()); // no score
        assert!(Dictionary::parse("CAT;abc\n").is_err()); // bad score
    }

    #[test]
    fn test_skips_non_alpha_words() {
        let dict = Dictionary::parse("CAT-DOG;50\nGOOD;50\n").unwrap();
        assert!(dict.bucket(7).is_none(), "CAT-DOG must be skipped");
        assert_eq!(dict.bucket(4).unwrap().words.len(), 1);
    }

    #[test]
    fn test_word_bytes_layout_matches_words() {
        let dict = Dictionary::parse("CAT;50\nDOG;50\n").unwrap();
        let bucket = dict.bucket(3).unwrap();
        for (id, word) in bucket.words.iter().enumerate() {
            for (pos, ch) in word.bytes().enumerate() {
                assert_eq!(bucket.word_bytes[id * 3 + pos], ch - b'A');
            }
        }
    }
}

#[cfg(test)]
mod union_tests {
    use super::*;

    fn dictionary() -> Dictionary {
        // Multiple blocks, a partial tail, and all 26 letters represented.
        let text: String = (0..137)
            .map(|i| {
                format!(
                    "{}{}{};50\n",
                    (b'A' + (i / 26) as u8) as char,
                    (b'A' + (i % 26) as u8) as char,
                    (b'A' + ((i * 7) % 26) as u8) as char
                )
            })
            .collect();
        Dictionary::parse(&text).unwrap()
    }

    #[test]
    fn grouped_unions_match_letter_indexes() {
        let dict = dictionary();
        let bucket = dict.bucket(3).unwrap();
        let all = (1u32 << 26) - 1;
        let mut masks = vec![0, all];
        for letter in 0..26 {
            masks.extend([1 << letter, all ^ (1 << letter)]);
        }
        // Exhaust every packed group, alone and combined with the singleton Q.
        for letters in UNION_GROUPS {
            for subset in 0..(1usize << letters.len()) {
                let mask = letters.iter().enumerate().fold(0, |mask, (bit, letter)| {
                    mask | (((subset >> bit) & 1) as u32) << letter
                });
                masks.extend([mask, mask | (1 << 16)]);
            }
        }
        let mut random = 42u32;
        for _ in 0..2000 {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            masks.push(random & all);
        }
        for pos in 0..3 {
            for &mask in &masks {
                let mut expected = vec![0; bucket.all.blocks().len()];
                for letter in 0..26 {
                    if mask & (1 << letter) != 0 {
                        for (dst, src) in expected
                            .iter_mut()
                            .zip(bucket.letter_bits[pos][letter].blocks())
                        {
                            *dst |= src;
                        }
                    }
                }
                let mut actual = vec![u64::MAX; expected.len()];
                bucket.letter_union(pos, mask, &mut actual);
                assert_eq!(actual, expected, "position {pos}, mask {mask:#x}");
                let mut bits = bucket.all.clone();
                let domain = bits.blocks_mut();
                for (i, block) in domain.iter_mut().enumerate() {
                    *block &= 0x5555555555555555u64.rotate_left(i as u32);
                }
                let before: u32 = domain.iter().map(|b| b.count_ones()).sum();
                let reference: Vec<u64> =
                    domain.iter().zip(&expected).map(|(d, f)| d & f).collect();
                let mut domain = crate::domain::CandidateSet::new(bits);
                let removed = bucket.intersect_letter_union(pos, mask, &mut domain);
                assert_eq!(domain.bits().blocks(), reference);
                assert_eq!(
                    removed,
                    before - reference.iter().map(|b| b.count_ones()).sum::<u32>()
                );
                assert_eq!(bucket.intersect_letter_union(pos, mask, &mut domain), 0);
            }
        }
    }

    #[test]
    fn union_tables_are_lazy_and_shared_between_clones() {
        let dict = dictionary();
        let bucket = dict.bucket(3).unwrap();
        let clone = bucket.clone();
        let mut out = vec![0; bucket.all.blocks().len()];
        bucket.letter_union(0, 1, &mut out);
        assert!(bucket.unions.iter().all(|cell| cell.get().is_none()));
        std::thread::scope(|scope| {
            for b in [bucket, &clone] {
                scope.spawn(move || {
                    let mut out = vec![0; b.all.blocks().len()];
                    b.letter_union(1, 3, &mut out);
                });
            }
        });
        assert!(bucket.unions[0].get().is_none());
        assert!(bucket.unions[2].get().is_none());
        assert!(std::ptr::eq(
            bucket.unions[1].get().unwrap().as_ptr(),
            clone.unions[1].get().unwrap().as_ptr()
        ));
    }
}
