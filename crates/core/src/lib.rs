//! Core data structures for the Orca crossword filler: bitset candidate
//! sets, the scored dictionary, and the parsed grid with its slots and
//! crossings.

pub mod bitset;
pub mod dict;
pub mod grid;

pub use bitset::BitSet;
pub use dict::Dictionary;
pub use grid::Grid;
