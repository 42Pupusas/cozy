//! Starting the user's mail client for a `mailto:` URL.

use opener::{ActivationToken, OpenError, Opener, Target};

/// Hands a composed message to a mail client.
pub trait MailLauncher: Send + Sync {
    fn launch(&self, mailto_url: &str, activation_token: Option<&str>) -> Result<(), OpenError>;
}

/// Launches through the system's `x-scheme-handler/mailto` association.
pub struct AssociatedMailClient;

impl MailLauncher for AssociatedMailClient {
    fn launch(&self, mailto_url: &str, activation_token: Option<&str>) -> Result<(), OpenError> {
        let target = Target::Url {
            raw: mailto_url.to_owned(),
            scheme: "mailto".to_owned(),
        };
        Opener::load()
            .with_activation(activation_token.map(ActivationToken::new))
            .try_open(&target)
    }
}
