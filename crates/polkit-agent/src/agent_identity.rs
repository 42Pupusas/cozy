//! Which user the helper should authenticate.
//!
//! Polkit hands the agent a list of identities that would satisfy the policy.
//! For an `auth_admin` action that list is the administrators, so it usually
//! names `root` and every member of the admin group. The agent must pick one
//! and tell the helper which.
//!
//! Preferring our own uid, when it appears, is what makes the prompt ask for
//! *your* password rather than root's -- the behaviour every desktop agent
//! has, and the difference between a prompt you can answer and one you
//! cannot.

/// A `unix-user` identity offered by polkit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    uid: u32,
    name: String,
}

impl AgentIdentity {
    /// Build an identity for `uid`, naming it from the password database.
    #[must_use]
    pub fn new(uid: u32, name: &str) -> Self {
        Self {
            uid,
            name: name.to_owned(),
        }
    }

    /// The login name, which is what the helper socket expects.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The numeric uid.
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    /// Look up the login name for `uid` in `/etc/passwd`.
    ///
    /// Reads the file rather than calling `getpwuid`, so this crate needs no
    /// libc binding of its own. That misses NSS-only users (LDAP, SSSD), so
    /// callers must tolerate `None` rather than treat it as impossible.
    #[must_use]
    pub fn lookup(uid: u32) -> Option<Self> {
        let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
        Self::from_passwd(&passwd, uid)
    }

    /// The same lookup against passwd text, so it is testable without
    /// touching the real database.
    #[must_use]
    pub fn from_passwd(passwd: &str, uid: u32) -> Option<Self> {
        for line in passwd.lines() {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _password = fields.next();
            let Some(entry_uid) = fields.next().and_then(|f| f.parse::<u32>().ok()) else {
                continue;
            };
            if entry_uid == uid {
                return Some(Self::new(uid, name));
            }
        }
        None
    }

    /// Choose which identity to authenticate from those polkit offered.
    ///
    /// Prefers `own_uid` so the user is asked for their own password. Falls
    /// back to the first identity offered, which is what a text agent does.
    #[must_use]
    pub fn choose(offered: &[Self], own_uid: u32) -> Option<Self> {
        offered
            .iter()
            .find(|id| id.uid == own_uid)
            .or_else(|| offered.first())
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small passwd file covering the shapes that occur in practice.
    const PASSWD: &str = "root:x:0:0::/root:/bin/bash\n\
                          bin:x:1:1::/:/usr/bin/nologin\n\
                          alice:x:1000:1000:Alice:/home/alice:/bin/bash\n\
                          bob:x:1001:1001::/home/bob:/bin/zsh\n";

    /// The everyday lookup.
    #[test]
    fn a_uid_resolves_to_its_login_name() {
        let id = AgentIdentity::from_passwd(PASSWD, 1000).expect("alice");
        assert_eq!(id.name(), "alice");
        assert_eq!(id.uid(), 1000);
    }

    /// Root is an ordinary entry and must resolve too.
    #[test]
    fn root_resolves() {
        let id = AgentIdentity::from_passwd(PASSWD, 0).expect("root");
        assert_eq!(id.name(), "root");
    }

    /// An absent uid must report absence rather than guessing a name.
    #[test]
    fn an_unknown_uid_resolves_to_nothing() {
        assert!(AgentIdentity::from_passwd(PASSWD, 4242).is_none());
    }

    /// A malformed line must be skipped, not abort the whole lookup -- a
    /// single bad entry cannot be allowed to hide every later user.
    #[test]
    fn a_malformed_line_does_not_hide_later_entries() {
        let passwd = "broken-line-with-no-fields\n\
                      also:bad\n\
                      alice:x:1000:1000::/home/alice:/bin/bash\n";
        let id = AgentIdentity::from_passwd(passwd, 1000).expect("alice");
        assert_eq!(id.name(), "alice");
    }

    /// The guard that makes the prompt answerable: when the policy would
    /// accept us, ask for our own password rather than root's.
    #[test]
    fn our_own_identity_is_preferred_over_root() {
        let offered = [
            AgentIdentity::new(0, "root"),
            AgentIdentity::new(1000, "alice"),
        ];
        let chosen = AgentIdentity::choose(&offered, 1000).expect("a choice");
        assert_eq!(chosen.name(), "alice", "must ask for the user's own password");
    }

    /// The order polkit sends must not change the choice.
    #[test]
    fn our_identity_wins_whatever_the_order() {
        let offered = [
            AgentIdentity::new(1000, "alice"),
            AgentIdentity::new(0, "root"),
        ];
        let chosen = AgentIdentity::choose(&offered, 1000).expect("a choice");
        assert_eq!(chosen.uid(), 1000);
    }

    /// When we are not an acceptable identity, fall back rather than refuse:
    /// the user may still know the root password.
    #[test]
    fn an_unlisted_user_falls_back_to_the_first_offer() {
        let offered = [
            AgentIdentity::new(0, "root"),
            AgentIdentity::new(999, "admin"),
        ];
        let chosen = AgentIdentity::choose(&offered, 1000).expect("a choice");
        assert_eq!(chosen.name(), "root");
    }

    /// No identities means nothing can be authenticated, which must be
    /// reported rather than papered over with a default.
    #[test]
    fn an_empty_offer_yields_no_choice() {
        assert!(AgentIdentity::choose(&[], 1000).is_none());
    }

    /// The live database must resolve our own uid, or the agent cannot run
    /// on this machine at all.
    #[test]
    fn our_real_uid_resolves_on_this_machine() {
        let uid = dbus::sys::DbusSys::effective_uid();
        let id = AgentIdentity::lookup(uid);
        assert!(id.is_some(), "uid {uid} must appear in /etc/passwd");
        assert!(!id.expect("checked").name().is_empty());
    }
}
