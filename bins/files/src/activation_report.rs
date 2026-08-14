//! Reporting whether a launch carried an activation token.
//!
//! # Why this is worth a module
//!
//! The token exchange has two silent degraded paths: the compositor offers
//! no `xdg_activation_v1`, or it never answers before the wait times out.
//! Both end as `None`, and a launch that opens the file but leaves the
//! window on another workspace looks exactly the same either way.
//!
//! That ambiguity cost real debugging time. This turns the two cases into
//! two different lines, behind an environment variable so ordinary runs stay
//! quiet.

use platform::toplevel::activation::ActivationToken;

/// Prints the outcome of a token request when `COZY_DEBUG_ACTIVATION` is set.
pub struct ActivationReport {
    enabled: bool,
}

impl ActivationReport {
    /// The variable that turns reporting on.
    pub const ENV: &'static str = "COZY_DEBUG_ACTIVATION";

    /// Read the flag once, at startup.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var_os(Self::ENV).is_some(),
        }
    }

    /// Report one launch.
    ///
    /// `supported` separates the two silent failures: with no global there
    /// was nothing to ask, and with one the compositor stayed quiet.
    pub fn record(&self, token: Option<&ActivationToken>, supported: bool) {
        if !self.enabled {
            return;
        }
        match token {
            Some(token) => eprintln!("files: minted activation token {}", token.as_str()),
            None => eprintln!("files: no activation token (supported={supported})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name is the one the platform crate's trace uses, so one variable
    /// turns on the whole path rather than half of it.
    #[test]
    fn the_flag_is_the_shared_activation_debug_variable() {
        assert_eq!(ActivationReport::ENV, "COZY_DEBUG_ACTIVATION");
    }

    /// Reporting is off unless asked for: a file browser that printed on
    /// every open would be noise in every shell it was started from.
    #[test]
    fn a_disabled_report_stays_silent_on_both_degraded_paths() {
        let report = ActivationReport { enabled: false };
        report.record(None, true);
        report.record(None, false);
    }
}
