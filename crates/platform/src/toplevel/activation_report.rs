//! [`ActivationReport`] -- saying which way a token request degraded.
//!
//! # Why this exists
//!
//! The exchange has two silent failure paths: the compositor offers no
//! `xdg_activation_v1`, or it never answers before the wait times out. Both
//! end as `None`, and a launch that opens the file but leaves the window on
//! another workspace looks identical either way.
//!
//! That ambiguity cost real debugging time once. This turns the two cases
//! into two different lines, behind an environment variable so ordinary runs
//! stay quiet.
//!
//! It lives beside the exchange rather than in one client because every
//! program that mints a token has the same question when a launch lands on
//! the wrong workspace.

use crate::toplevel::activation::ActivationToken;

/// Prints the outcome of a token request when `COZY_DEBUG_ACTIVATION` is set.
#[derive(Debug, Clone, Copy)]
pub struct ActivationReport {
    enabled: bool,
    program: &'static str,
}

impl ActivationReport {
    /// The variable that turns reporting on.
    ///
    /// The same name the platform crate's own activation trace uses, so one
    /// variable lights up the whole path rather than half of it.
    pub const ENV: &'static str = "COZY_DEBUG_ACTIVATION";

    /// Read the flag once, at startup.
    ///
    /// `program` prefixes each line, so a session running several of these
    /// binaries can tell which one reported.
    #[must_use]
    pub fn from_env(program: &'static str) -> Self {
        Self {
            enabled: std::env::var_os(Self::ENV).is_some(),
            program,
        }
    }

    /// A reporter that never prints, for tests and for callers that do not
    /// want the diagnostic at all.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            program: "",
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
        let program = self.program;
        match token {
            Some(token) => {
                eprintln!("{program}: minted activation token {}", token.as_str());
            }
            None => eprintln!("{program}: no activation token (supported={supported})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ActivationReport;

    /// One variable turns on both this and the wire-level trace.
    #[test]
    fn the_flag_is_the_shared_activation_debug_variable() {
        assert_eq!(ActivationReport::ENV, "COZY_DEBUG_ACTIVATION");
    }

    /// Reporting is off unless asked for: a browser that printed on every
    /// open would be noise in every shell it was started from.
    #[test]
    fn a_disabled_report_stays_silent_on_both_degraded_paths() {
        let report = ActivationReport::disabled();
        report.record(None, true);
        report.record(None, false);
    }
}
