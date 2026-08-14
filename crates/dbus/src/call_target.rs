//! Where a D-Bus method call is addressed.
//!
//! A call names four things: the bus name that serves it, the object on that
//! peer, the interface the member belongs to, and the member itself. They are
//! always supplied together and are meaningless apart, so they travel as one
//! value rather than as four adjacent `&str` parameters that a caller can
//! silently transpose.

/// The destination, object path, interface, and member of one method call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallTarget<'a> {
    destination: &'a str,
    path: &'a str,
    interface: &'a str,
    member: &'a str,
}

impl<'a> CallTarget<'a> {
    #[must_use]
    pub const fn new(
        destination: &'a str,
        path: &'a str,
        interface: &'a str,
        member: &'a str,
    ) -> Self {
        Self {
            destination,
            path,
            interface,
            member,
        }
    }

    #[must_use]
    pub const fn destination(&self) -> &'a str {
        self.destination
    }

    #[must_use]
    pub const fn path(&self) -> &'a str {
        self.path
    }

    #[must_use]
    pub const fn interface(&self) -> &'a str {
        self.interface
    }

    #[must_use]
    pub const fn member(&self) -> &'a str {
        self.member
    }
}

#[cfg(test)]
mod tests {
    use super::CallTarget;

    #[test]
    fn every_field_reads_back_as_given() {
        let target = CallTarget::new(
            "org.example.Service",
            "/org/example",
            "org.example.Iface",
            "Method",
        );
        assert_eq!(target.destination(), "org.example.Service");
        assert_eq!(target.path(), "/org/example");
        assert_eq!(target.interface(), "org.example.Iface");
        assert_eq!(target.member(), "Method");
    }

    /// The four fields are all strings, so a transposition compiles. Equality
    /// is what a test can use to catch one.
    #[test]
    fn two_targets_differing_in_one_field_are_not_equal() {
        let target = CallTarget::new("dest", "/path", "iface", "Member");
        let swapped = CallTarget::new("dest", "/path", "Member", "iface");
        assert_ne!(target, swapped);
    }
}
