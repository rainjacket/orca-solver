//! Dictionary loading and indexing. Words are grouped by length into
//! [`LengthBucket`]s with precomputed `letter_bits[position][letter]` bitset
//! indexes for O(1) candidate filtering during constraint propagation.

use crate::bitset::BitSet;
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// All words of a given length, with precomputed bitset indexes for fast filtering.
#[derive(Debug, Clone)]
pub struct LengthBucket {
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
                    let num_words = self.all.len();
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
        let content = fs::read_to_string(path)?;
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
}
