//! Emitter options.

/// Emitter tuning knobs. Kept minimal on purpose — formatting fidelity lives
/// in the IR trivia, not in global flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Spaces per indent level.
    pub indent: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { indent: 2 }
    }
}
