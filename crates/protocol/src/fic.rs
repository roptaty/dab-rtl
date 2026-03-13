/// FIC (Fast Information Channel) handler.
///
/// The FIC carries 3 FIBs per CIF (Common Interleaved Frame).
/// Each FIB is 32 bytes (30 content + 2 CRC).

use crate::ensemble::Ensemble;
use crate::fib::FibParser;

pub struct FicHandler {
    parser: FibParser,
}

impl FicHandler {
    pub fn new() -> Self {
        FicHandler { parser: FibParser::new() }
    }

    /// Process decoded FIC bytes.
    ///
    /// `fic_bytes` should be exactly 96 bytes (3 × 32-byte FIBs).
    /// Returns a reference to the accumulated ensemble info.
    pub fn process_fic_bytes(&mut self, fic_bytes: &[u8]) -> &Ensemble {
        const FIB_SIZE: usize = 32;
        let n_fibs = fic_bytes.len() / FIB_SIZE;

        for i in 0..n_fibs {
            let fib = &fic_bytes[i * FIB_SIZE..(i + 1) * FIB_SIZE];
            if fib.len() == FIB_SIZE {
                self.parser.parse_fib(fib);
            }
        }

        &self.parser.ensemble
    }

    /// Current ensemble snapshot.
    pub fn ensemble(&self) -> &Ensemble {
        &self.parser.ensemble
    }
}

impl Default for FicHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_default_ensemble() {
        let mut handler = FicHandler::new();
        let ens = handler.process_fic_bytes(&[]);
        assert_eq!(ens.id, 0);
        assert!(ens.services.is_empty());
    }

    #[test]
    fn three_zero_fibs_do_not_panic() {
        let mut handler = FicHandler::new();
        let data = vec![0u8; 96];
        handler.process_fic_bytes(&data);
    }
}
