use serde::{Deserialize, Serialize};

/// Exact-ratio fee, e.g. 997/1000 for a 0.3% fee.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fee {
    pub numerator: u32,
    pub denominator: u32,
}

impl Fee {
    pub const fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if denominator == 0 {
            None
        } else {
            Some(Self {
                numerator,
                denominator,
            })
        }
    }

    pub const ZERO: Fee = Fee {
        numerator: 1,
        denominator: 1,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_denominator_is_rejected() {
        assert_eq!(Fee::new(997, 0), None);
    }
}
