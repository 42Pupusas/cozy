//! One `BeginAuthentication` call, decoded.
//!
//! Polkit sends six arguments, signature `sssa{ss}sa(sa{sv})`:
//!
//! ```text
//! action_id    the action being authorised, e.g. org.freedesktop.systemd1.manage-units
//! message      the sentence to show the user
//! icon_name    a themed icon, often empty
//! details      key/value pairs about the request
//! cookie       identifies this request to the helper
//! identities   who may authenticate, as (s, a{sv}) structs
//! ```
//!
//! The identity struct is the awkward part: the string is a kind
//! (`"unix-user"` or `"unix-group"`) and the dictionary holds the `uid` or
//! `gid` as a variant. Only `unix-user` can be authenticated by the helper,
//! so a group identity is dropped rather than misread as a user.

use crate::agent_identity::AgentIdentity;
use dbus::value::Value;

/// A decoded request to authenticate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRequest {
    action_id: String,
    message: String,
    cookie: String,
    identities: Vec<AgentIdentity>,
}

impl AuthRequest {
    /// Decode the arguments of a `BeginAuthentication` call.
    ///
    /// Returns `None` when the shape is wrong, which must be an error reply
    /// rather than a guess: authenticating for the wrong action, or against
    /// the wrong cookie, is worse than refusing.
    #[must_use]
    pub fn from_args(args: &[Value]) -> Option<Self> {
        let action_id = args.first()?.clone().into_inner().as_display_string()?;
        let message = args.get(1)?.clone().into_inner().as_display_string()?;
        let cookie = args.get(4)?.clone().into_inner().as_display_string()?;
        let identities = Self::decode_identities(args.get(5)?);
        Some(Self {
            action_id,
            message,
            cookie,
            identities,
        })
    }

    /// The action polkit is guarding.
    #[must_use]
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    /// The sentence polkit wants shown to the user.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The cookie identifying this request to the helper.
    #[must_use]
    pub fn cookie(&self) -> &str {
        &self.cookie
    }

    /// Everyone who may authenticate for this request.
    #[must_use]
    pub fn identities(&self) -> &[AgentIdentity] {
        &self.identities
    }

    /// Pick the identity to authenticate, preferring `own_uid`.
    #[must_use]
    pub fn choose_identity(&self, own_uid: u32) -> Option<AgentIdentity> {
        AgentIdentity::choose(&self.identities, own_uid)
    }

    /// Decode `a(sa{sv})` into the users it names, skipping anything that is
    /// not a `unix-user` with a readable uid.
    fn decode_identities(value: &Value) -> Vec<AgentIdentity> {
        let unwrapped = value.clone().into_inner();
        let Some(items) = unwrapped.as_items() else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(Self::decode_identity)
            .collect()
    }

    /// Decode one `(s, a{sv})` identity struct.
    fn decode_identity(item: &Value) -> Option<AgentIdentity> {
        let unwrapped = item.clone().into_inner();
        let fields = unwrapped.as_items()?;
        let kind = fields.first()?.clone().into_inner().as_display_string()?;
        if kind != "unix-user" {
            return None;
        }
        let uid = Self::lookup_u32(fields.get(1)?, "uid")?;
        AgentIdentity::lookup(uid).or_else(|| Some(AgentIdentity::new(uid, &uid.to_string())))
    }

    /// Read `key` out of an `a{sv}` as a number, whatever integer width the
    /// sender chose.
    fn lookup_u32(dict: &Value, key: &str) -> Option<u32> {
        let unwrapped = dict.clone().into_inner();
        let entries = unwrapped.as_items()?;
        for entry in entries {
            let Value::DictEntry(entry_key, entry_value) = entry else {
                continue;
            };
            if entry_key.clone().into_inner().as_display_string()? == key {
                let number = entry_value.clone().into_inner().as_u64()?;
                return u32::try_from(number).ok();
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `unix-user` identity struct as polkit sends it.
    fn identity(uid: u32) -> Value {
        Value::Struct(vec![
            Value::Str("unix-user".into()),
            Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("uid".into())),
                Box::new(Value::Variant(Box::new(Value::U32(uid)))),
            )]),
        ])
    }

    /// A full, well-formed call as polkit would send it.
    fn begin_authentication_args(identities: Vec<Value>) -> Vec<Value> {
        vec![
            Value::Str("org.freedesktop.systemd1.manage-units".into()),
            Value::Str("Authentication is required to manage system services.".into()),
            Value::Str(String::new()),
            Value::Array(vec![]),
            Value::Str("cookie-12345".into()),
            Value::Array(identities),
        ]
    }

    /// The everyday call must decode completely.
    #[test]
    fn a_real_request_decodes_every_field() {
        let args = begin_authentication_args(vec![identity(0), identity(1000)]);
        let request = AuthRequest::from_args(&args).expect("a well-formed request");
        assert_eq!(request.action_id(), "org.freedesktop.systemd1.manage-units");
        assert!(request.message().starts_with("Authentication is required"));
        assert_eq!(request.cookie(), "cookie-12345");
        assert_eq!(request.identities().len(), 2);
    }

    /// The cookie is what authorises the request; reading the wrong field
    /// would authenticate against someone else's pending action.
    #[test]
    fn the_cookie_comes_from_the_fifth_argument() {
        let args = begin_authentication_args(vec![identity(1000)]);
        let request = AuthRequest::from_args(&args).expect("well-formed");
        assert_eq!(request.cookie(), "cookie-12345");
        assert_ne!(request.cookie(), request.action_id());
    }

    /// A group identity cannot be authenticated by the helper and must be
    /// dropped rather than read as a user with the gid as a uid.
    #[test]
    fn a_group_identity_is_ignored() {
        let group = Value::Struct(vec![
            Value::Str("unix-group".into()),
            Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("gid".into())),
                Box::new(Value::Variant(Box::new(Value::U32(998)))),
            )]),
        ]);
        let args = begin_authentication_args(vec![group, identity(1000)]);
        let request = AuthRequest::from_args(&args).expect("well-formed");
        assert_eq!(request.identities().len(), 1, "only the user survives");
        assert_eq!(request.identities()[0].uid(), 1000);
    }

    /// Our own uid must be chosen, so the prompt asks for our password.
    #[test]
    fn our_own_identity_is_chosen_when_offered() {
        let args = begin_authentication_args(vec![identity(0), identity(1000)]);
        let request = AuthRequest::from_args(&args).expect("well-formed");
        let chosen = request.choose_identity(1000).expect("a choice");
        assert_eq!(chosen.uid(), 1000);
    }

    /// A malformed body must be refused outright.
    #[test]
    fn a_truncated_request_is_refused() {
        let short = vec![Value::Str("action".into()), Value::Str("message".into())];
        assert!(AuthRequest::from_args(&short).is_none());
        assert!(AuthRequest::from_args(&[]).is_none());
    }

    /// An empty identity list decodes rather than failing, but offers no
    /// choice -- the caller must handle that, not be handed a default.
    #[test]
    fn a_request_with_no_identities_offers_no_choice() {
        let args = begin_authentication_args(vec![]);
        let request = AuthRequest::from_args(&args).expect("well-formed");
        assert!(request.identities().is_empty());
        assert!(request.choose_identity(1000).is_none());
    }

    /// A uid sent as a different integer width must still decode; the
    /// signature says variant, so the sender picks the type.
    #[test]
    fn a_uid_of_any_integer_width_decodes() {
        let wide = Value::Struct(vec![
            Value::Str("unix-user".into()),
            Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("uid".into())),
                Box::new(Value::Variant(Box::new(Value::U64(1000)))),
            )]),
        ]);
        let args = begin_authentication_args(vec![wide]);
        let request = AuthRequest::from_args(&args).expect("well-formed");
        assert_eq!(request.identities()[0].uid(), 1000);
    }

    /// An identity whose dictionary lacks a uid must be skipped, not
    /// defaulted to 0 -- which is root.
    #[test]
    fn an_identity_without_a_uid_is_skipped_rather_than_defaulting_to_root() {
        let broken = Value::Struct(vec![
            Value::Str("unix-user".into()),
            Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("something-else".into())),
                Box::new(Value::Variant(Box::new(Value::U32(5)))),
            )]),
        ]);
        let args = begin_authentication_args(vec![broken]);
        let request = AuthRequest::from_args(&args).expect("well-formed");
        assert!(
            request.identities().is_empty(),
            "a uid-less identity must never become uid 0"
        );
    }

    /// A uid with no passwd entry must still be usable, since NSS users
    /// (LDAP, SSSD) do not appear in /etc/passwd.
    #[test]
    fn an_unknown_uid_still_yields_an_identity() {
        let args = begin_authentication_args(vec![identity(4242)]);
        let request = AuthRequest::from_args(&args).expect("well-formed");
        assert_eq!(request.identities().len(), 1);
        assert_eq!(request.identities()[0].uid(), 4242);
    }
}
