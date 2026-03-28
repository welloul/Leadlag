//! Hysteresis state machine for lead-lag role flipping.
//!
//! Prevents rapid role switching due to microstructural noise by requiring
//! the new lead to maintain dominance for a minimum number of consecutive
//! time grids before validating the flip.

/// Current lead-lag role assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadRole {
    /// Exchange A is the lead (sensor).
    ExchangeA,
    /// Exchange B is the lead (sensor).
    ExchangeB,
    /// No lead determined yet (insufficient data).
    Undetermined,
}

impl LeadRole {
    /// Get the laggard (actor) venue.
    pub fn laggard(&self) -> LeadRole {
        match self {
            LeadRole::ExchangeA => LeadRole::ExchangeB,
            LeadRole::ExchangeB => LeadRole::ExchangeA,
            LeadRole::Undetermined => LeadRole::Undetermined,
        }
    }
}

/// Hysteresis state machine for role-flip validation.
///
/// A role flip is only validated if:
/// 1. The new lead's correlation exceeds the current lead's by `threshold_margin`
/// 2. The new lead maintains dominance for `min_consecutive` consecutive checks
#[derive(Clone)]
pub struct Hysteresis {
    /// Current validated lead role.
    current_lead: LeadRole,
    /// Correlation of the current lead.
    current_r: f64,
    /// Candidate lead role (being evaluated for flip).
    candidate_lead: LeadRole,
    /// Correlation of the candidate lead.
    candidate_r: f64,
    /// Number of consecutive times the candidate has been dominant.
    candidate_streak: u32,
    /// Minimum correlation margin required for a flip.
    threshold_margin: f64,
    /// Minimum consecutive dominance required for a flip.
    min_consecutive: u32,
}

impl Hysteresis {
    /// Create a new hysteresis state machine.
    ///
    /// # Arguments
    /// * `threshold_margin` - Minimum R difference to consider a flip (e.g., 0.10)
    /// * `min_consecutive` - Minimum consecutive dominance ticks (e.g., 3)
    pub fn new(threshold_margin: f64, min_consecutive: u32) -> Self {
        Self {
            current_lead: LeadRole::Undetermined,
            current_r: 0.0,
            candidate_lead: LeadRole::Undetermined,
            candidate_r: 0.0,
            candidate_streak: 0,
            threshold_margin,
            min_consecutive,
        }
    }

    /// Update the state machine with new correlation values.
    ///
    /// Returns the new lead role if a flip was validated.
    /// Flip based on consistent leader change (streak), not magnitude.
    pub fn update(&mut self, r_a: f64, r_b: f64) -> Option<LeadRole> {
        // Determine which exchange has higher correlation
        let (new_lead, new_r) = if r_a > r_b {
            (LeadRole::ExchangeA, r_a)
        } else if r_b > r_a {
            (LeadRole::ExchangeB, r_b)
        } else {
            // Tie - no change
            return None;
        };

        // If undetermined, set initial lead (but don't return a signal)
        if self.current_lead == LeadRole::Undetermined {
            self.current_lead = new_lead;
            self.current_r = new_r;
            return None; // Don't generate signal on initial lead determination
        }

        // Check if the new lead is the same as current
        if new_lead == self.current_lead {
            // Current lead is still dominant, reset candidate
            self.current_r = new_r;
            self.candidate_streak = 0;
            return None;
        }

        // New lead is different from current
        // Flip based on consistent leader change (streak), not magnitude
        if self.candidate_lead == new_lead {
            // Same candidate as before, increment streak
            self.candidate_streak += 1;
        } else {
            // New candidate, reset streak
            self.candidate_lead = new_lead;
            self.candidate_r = new_r;
            self.candidate_streak = 1;
        }

        // Check if we've met the consecutive requirement
        if self.candidate_streak >= self.min_consecutive {
            // Flip validated!
            self.current_lead = new_lead;
            self.current_r = new_r;
            self.candidate_streak = 0;
            return Some(new_lead);
        }

        None
    }

    /// Get the current lead role.
    pub fn current_lead(&self) -> LeadRole {
        self.current_lead
    }

    /// Get the current laggard role.
    pub fn current_laggard(&self) -> LeadRole {
        self.current_lead.laggard()
    }

    /// Get the current lead's correlation.
    pub fn current_r(&self) -> f64 {
        self.current_r
    }

    /// Check if a flip is pending (candidate has some streak).
    pub fn is_flip_pending(&self) -> bool {
        self.candidate_streak > 0
    }

    /// Reset the state machine.
    pub fn clear(&mut self) {
        self.current_lead = LeadRole::Undetermined;
        self.current_r = 0.0;
        self.candidate_lead = LeadRole::Undetermined;
        self.candidate_r = 0.0;
        self.candidate_streak = 0;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_lead_determination() {
        let mut hyst = Hysteresis::new(0.10, 3);

        // First update should set initial lead but not return a signal
        let result = hyst.update(0.9, 0.8);
        assert_eq!(result, None); // No signal on initial lead determination
        assert_eq!(hyst.current_lead(), LeadRole::ExchangeA);
    }

    #[test]
    fn test_no_flip_below_threshold() {
        let mut hyst = Hysteresis::new(0.10, 3);

        hyst.update(0.9, 0.8); // A leads

        // B is higher but not by enough margin
        let result = hyst.update(0.85, 0.88);
        assert_eq!(result, None);
        assert_eq!(hyst.current_lead(), LeadRole::ExchangeA);
    }

    #[test]
    fn test_flip_after_consecutive_dominance() {
        // Use a smaller threshold margin to test flip behavior
        // With threshold_margin = 0.05, we need B's correlation > 0.9 + 0.05 = 0.95
        let mut hyst = Hysteresis::new(0.05, 3);
        hyst.update(0.9, 0.8); // A leads with r=0.9

        // B becomes dominant by margin (0.96 > 0.95)
        hyst.update(0.80, 0.96); // streak = 1 (0.96 > 0.95)
        assert_eq!(hyst.current_lead(), LeadRole::ExchangeA); // Not yet

        hyst.update(0.80, 0.96); // streak = 2
        assert_eq!(hyst.current_lead(), LeadRole::ExchangeA); // Not yet

        let result = hyst.update(0.80, 0.96); // streak = 3, flip!
        assert_eq!(result, Some(LeadRole::ExchangeB));
        assert_eq!(hyst.current_lead(), LeadRole::ExchangeB);
    }

    #[test]
    fn test_no_flip_when_current_lead_reasserts() {
        let mut hyst = Hysteresis::new(0.10, 3);

        hyst.update(0.9, 0.8); // A leads

        // B becomes dominant (r_b > r_a), starting candidate streak
        hyst.update(0.80, 0.95); // streak = 1
        hyst.update(0.80, 0.95); // streak = 2

        // A reasserts dominance — streak resets
        hyst.update(0.95, 0.80); // streak = 0 (reset)

        // B becomes dominant again — streak starts fresh at 1
        hyst.update(0.80, 0.95); // streak = 1 (not enough for flip with min_consecutive=3)
        assert_eq!(hyst.current_lead(), LeadRole::ExchangeA);
    }

    #[test]
    fn test_streak_resets_on_interruption() {
        let mut hyst = Hysteresis::new(0.10, 3);

        hyst.update(0.9, 0.8); // A leads

        // B becomes dominant
        hyst.update(0.80, 0.95); // streak = 1
        hyst.update(0.80, 0.95); // streak = 2

        // A becomes dominant again (interrupts streak)
        hyst.update(0.95, 0.80);

        // B becomes dominant again
        hyst.update(0.80, 0.95); // streak = 1 (reset)
        assert_eq!(hyst.current_lead(), LeadRole::ExchangeA);
    }

    #[test]
    fn test_laggard_is_opposite() {
        assert_eq!(LeadRole::ExchangeA.laggard(), LeadRole::ExchangeB);
        assert_eq!(LeadRole::ExchangeB.laggard(), LeadRole::ExchangeA);
    }

    #[test]
    fn test_clear() {
        let mut hyst = Hysteresis::new(0.10, 3);
        hyst.update(0.9, 0.8);
        hyst.clear();

        assert_eq!(hyst.current_lead(), LeadRole::Undetermined);
    }
}
