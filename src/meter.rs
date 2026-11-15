//! A deterministic work budget. The input is hostile, so a call must not be able to burn unbounded time even
//! when no single loop is large: the same object can be re-parsed once per page, for instance, which is
//! quadratic in the input. Every stage that does work proportional to something an attacker controls charges
//! the meter, and the call fails with [`Limit::Work`](crate::Limit::Work) when the budget runs out.
//!
//! One unit is roughly a nanosecond of work on a modern core, so the numbers read as time, but the count does
//! not depend on the machine: the same input costs the same everywhere, which keeps tests and the differential
//! gate reproducible. The weights below are measured, rough, and relative to each other.
use crate::{Failure, Limit};
use std::cell::Cell;

/// Cost per byte (or per item) of each kind of work.
pub(crate) mod cost {
    /// Parsing an object: tokenising and building values.
    pub const PARSE: u64 = 10;
    /// Scanning bytes for a keyword.
    pub const SCAN: u64 = 1;
    /// Reading compressed bytes into the inflater.
    pub const INFLATE_IN: u64 = 1;
    /// Producing a decompressed byte.
    pub const INFLATE_OUT: u64 = 3;
    /// Undoing a predictor on a decompressed byte.
    pub const PREDICT: u64 = 2;
    /// Hashing or ciphering a byte (MD5, SHA-2, AES, RC4).
    pub const CRYPTO: u64 = 5;
    /// Visiting one node of the page tree.
    pub const NODE: u64 = 20;
}

pub(crate) struct Meter {
    used: Cell<u64>,
    max: u64,
    /// Parsed values alive at once, and the most allowed ([`Limit::Values`](crate::Limit::Values)). Work bounds
    /// time; this bounds memory, since a small compressed stream can inflate to millions of tiny values. Code
    /// that parses and then drops objects gives their values back with [`release_values`](Self::release_values).
    values: Cell<usize>,
    max_values: usize,
    /// Set once the limit is passed, and never cleared, so giving values back cannot hide it.
    values_hit: Cell<bool>,
}

impl Meter {
    /// A meter with a work budget and no limit on parsed values.
    pub fn new(max: u64) -> Meter {
        Meter {
            used: Cell::new(0),
            max,
            values: Cell::new(0),
            max_values: usize::MAX,
            values_hit: Cell::new(false),
        }
    }

    /// Limits the number of parsed values to `max_values`.
    pub fn with_max_values(self, max_values: usize) -> Meter {
        Meter { max_values, ..self }
    }

    /// Counts one parsed value. `false` once the values alive pass the limit; the parser then stops at once.
    pub fn take_value(&self) -> bool {
        let values = self.values.get().saturating_add(1);
        self.values.set(values);
        if values > self.max_values {
            self.values_hit.set(true);
        }
        !self.values_hit.get()
    }

    /// The values counted as alive, to pass to [`release_values`](Self::release_values) later.
    pub fn values(&self) -> usize {
        self.values.get()
    }

    /// Gives back every value counted since `mark`, once the objects they belong to are dropped. Objects the
    /// shared cache keeps are given back too: the cache has its own bound.
    pub fn release_values(&self, mark: usize) {
        self.values.set(self.values.get().min(mark));
    }

    /// Whether the parsed-value limit was ever passed. Every parse fails from then on, whatever the caller made
    /// of the failure, so [`probe`](crate::probe) reports the limit rather than that failure.
    pub fn values_exceeded(&self) -> bool {
        self.values_hit.get()
    }

    /// Spends `units`. `Err` once the total passes the budget.
    pub fn charge(&self, units: u64) -> Result<(), Failure> {
        let used = self.used.get().saturating_add(units);
        self.used.set(used);
        if used > self.max {
            Err(Failure::LimitExceeded(Limit::Work))
        } else {
            Ok(())
        }
    }

    pub fn used(&self) -> u64 {
        self.used.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spends_up_to_the_budget_and_then_fails() {
        let m = Meter::new(10);
        assert!(m.charge(4).is_ok());
        assert!(m.charge(6).is_ok());
        assert_eq!(m.used(), 10);
        assert_eq!(m.charge(1), Err(Failure::LimitExceeded(Limit::Work)));
        // It stays failed.
        assert_eq!(m.charge(0), Err(Failure::LimitExceeded(Limit::Work)));
    }

    #[test]
    fn a_huge_charge_saturates_instead_of_wrapping() {
        let m = Meter::new(u64::MAX - 1);
        assert!(m.charge(u64::MAX).is_err());
        assert_eq!(m.used(), u64::MAX);
    }

    #[test]
    fn counts_parsed_values_up_to_the_limit() {
        let m = Meter::new(u64::MAX).with_max_values(2);
        assert!(m.take_value() && m.take_value());
        assert!(!m.values_exceeded());
        assert!(!m.take_value());
        assert!(m.values_exceeded());
        assert!(!m.take_value());
        assert!(Meter::new(u64::MAX).take_value());
    }

    #[test]
    fn released_values_can_be_taken_again_but_a_hit_stays() {
        let m = Meter::new(u64::MAX).with_max_values(2);
        let mark = m.values();
        assert!(m.take_value() && m.take_value());
        m.release_values(mark);
        assert_eq!(m.values(), 0);
        assert!(m.take_value() && m.take_value());
        assert!(!m.take_value());
        m.release_values(mark);
        assert!(m.values_exceeded());
        assert!(!m.take_value());
    }
}
