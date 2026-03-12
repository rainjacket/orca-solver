//! Shared test utilities for the solver crate.

#[cfg(test)]
use orca_core::dict::Dictionary;
#[cfg(test)]
use orca_core::grid::Grid;

#[cfg(test)]
use crate::state::{init_domains, SolverState};

/// Canonical test dictionary used across solver tests.
/// 16 three-letter words providing enough variety for 3x3 grid tests.
#[cfg(test)]
pub fn test_dict() -> Dictionary {
    Dictionary::parse(
        "CAT;50\nCAR;45\nCOT;40\nDOG;30\nDOT;35\nARC;25\nRAT;30\nTAR;35\nOAT;20\nATE;15\nTOE;10\nORE;10\nACE;10\nAGE;10\nROT;10\nOAR;10\n",
    )
    .unwrap()
}

/// Initialize solver state from a grid and dictionary.
#[cfg(test)]
pub fn init_state(grid: &Grid, dict: &Dictionary) -> SolverState {
    let domains = init_domains(grid, dict);
    SolverState::new(domains)
}
