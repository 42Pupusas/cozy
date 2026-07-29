//! Building the introspection XML for an object.
//!
//! `org.freedesktop.DBus.Introspectable.Introspect` returns the methods,
//! signals, and properties of each interface an object implements, plus
//! any child object names nested under its path.
//!
//! Kept separate from [`crate::registry::ObjectRegistry`] and
//! [`crate::router::MethodCallRouter`] on purpose: those two track *by
//! name* what a path implements and how a call against it dispatches, but
//! neither one knows argument names or D-Bus signal/property shapes --
//! that's descriptive metadata a caller assembles once per interface
//! (typically as a `const fn` returning a fixed [`InterfaceDescriptor`],
//! e.g. one per portal interface) and feeds in here, independently of
//! whatever object happens to be registered under it at runtime.

use std::fmt::Write;

/// One method or signal argument.
///
/// A name (every interface in the D-Bus spec bothers to name its
/// arguments for documentation purposes, even though introspection
/// consumers don't strictly require it) and its signature, e.g.
/// `("options", "a{sv}")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    name: String,
    signature: String,
}

impl Argument {
    /// A new argument descriptor.
    pub fn new(name: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            signature: signature.into(),
        }
    }

    /// Render as a method `<arg>`, which (unlike a signal's) must state
    /// its `direction`.
    fn to_method_arg_xml(&self, direction: &str) -> String {
        format!(
            "        <arg name=\"{}\" type=\"{}\" direction=\"{direction}\"/>\n",
            self.name, self.signature
        )
    }

    /// Render as a signal `<arg>`: signals have no `direction` attribute
    /// in the spec (every signal argument is implicitly outbound).
    fn to_signal_arg_xml(&self) -> String {
        format!(
            "        <arg name=\"{}\" type=\"{}\"/>\n",
            self.name, self.signature
        )
    }
}

/// Whether a property can be read, written, or both -- the `access`
/// attribute on a `<property>` element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyAccess {
    Read,
    Write,
    ReadWrite,
}

impl PropertyAccess {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::ReadWrite => "readwrite",
        }
    }
}

/// One method: its name, input arguments (`direction="in"`), and output
/// arguments (`direction="out"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDescriptor {
    name: String,
    in_args: Vec<Argument>,
    out_args: Vec<Argument>,
}

impl MethodDescriptor {
    /// A method taking `in_args` and returning `out_args`.
    pub fn new(name: impl Into<String>, in_args: Vec<Argument>, out_args: Vec<Argument>) -> Self {
        Self {
            name: name.into(),
            in_args,
            out_args,
        }
    }

    fn to_xml(&self) -> String {
        let mut xml = format!("      <method name=\"{}\">\n", self.name);
        for arg in &self.in_args {
            xml.push_str(&arg.to_method_arg_xml("in"));
        }
        for arg in &self.out_args {
            xml.push_str(&arg.to_method_arg_xml("out"));
        }
        xml.push_str("      </method>\n");
        xml
    }
}

/// One signal: its name and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalDescriptor {
    name: String,
    args: Vec<Argument>,
}

impl SignalDescriptor {
    pub fn new(name: impl Into<String>, args: Vec<Argument>) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }

    fn to_xml(&self) -> String {
        if self.args.is_empty() {
            return format!("      <signal name=\"{}\"/>\n", self.name);
        }
        let mut xml = format!("      <signal name=\"{}\">\n", self.name);
        for arg in &self.args {
            xml.push_str(&arg.to_signal_arg_xml());
        }
        xml.push_str("      </signal>\n");
        xml
    }
}

/// One property: its name, signature, and access mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyDescriptor {
    name: String,
    signature: String,
    access: PropertyAccess,
}

impl PropertyDescriptor {
    pub fn new(
        name: impl Into<String>,
        signature: impl Into<String>,
        access: PropertyAccess,
    ) -> Self {
        Self {
            name: name.into(),
            signature: signature.into(),
            access,
        }
    }

    fn to_xml(&self) -> String {
        format!(
            "      <property name=\"{}\" type=\"{}\" access=\"{}\"/>\n",
            self.name,
            self.signature,
            self.access.as_str()
        )
    }
}

/// One interface's full shape: its methods, signals, and properties.
///
/// Built with the chained `method`/`signal`/`property` calls (a fixed
/// descriptor is typically assembled once, e.g. as a `const fn` per
/// portal interface, and reused across every object that implements it).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InterfaceDescriptor {
    name: String,
    methods: Vec<MethodDescriptor>,
    signals: Vec<SignalDescriptor>,
    properties: Vec<PropertyDescriptor>,
}

impl InterfaceDescriptor {
    /// A new, empty interface named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Add a method.
    #[must_use]
    pub fn method(mut self, method: MethodDescriptor) -> Self {
        self.methods.push(method);
        self
    }

    /// Add a signal.
    #[must_use]
    pub fn signal(mut self, signal: SignalDescriptor) -> Self {
        self.signals.push(signal);
        self
    }

    /// Add a property.
    #[must_use]
    pub fn property(mut self, property: PropertyDescriptor) -> Self {
        self.properties.push(property);
        self
    }

    /// The standard `org.freedesktop.DBus.Introspectable` interface, which
    /// [`NodeIntrospection::to_xml`] adds to every node automatically --
    /// every object this crate's [`crate::registry::ObjectRegistry`]
    /// tracks answers this whether or not the caller registered it
    /// explicitly.
    #[must_use]
    pub fn introspectable() -> Self {
        Self::new("org.freedesktop.DBus.Introspectable").method(MethodDescriptor::new(
            "Introspect",
            vec![],
            vec![Argument::new("xml_data", "s")],
        ))
    }

    /// The standard `org.freedesktop.DBus.Properties` interface, likewise
    /// added to every node automatically.
    #[must_use]
    pub fn properties() -> Self {
        Self::new("org.freedesktop.DBus.Properties")
            .method(MethodDescriptor::new(
                "Get",
                vec![
                    Argument::new("interface_name", "s"),
                    Argument::new("property_name", "s"),
                ],
                vec![Argument::new("value", "v")],
            ))
            .method(MethodDescriptor::new(
                "Set",
                vec![
                    Argument::new("interface_name", "s"),
                    Argument::new("property_name", "s"),
                    Argument::new("value", "v"),
                ],
                vec![],
            ))
            .method(MethodDescriptor::new(
                "GetAll",
                vec![Argument::new("interface_name", "s")],
                vec![Argument::new("properties", "a{sv}")],
            ))
            .signal(SignalDescriptor::new(
                "PropertiesChanged",
                vec![
                    Argument::new("interface_name", "s"),
                    Argument::new("changed_properties", "a{sv}"),
                    Argument::new("invalidated_properties", "as"),
                ],
            ))
    }

    fn to_xml(&self) -> String {
        let mut xml = format!("    <interface name=\"{}\">\n", self.name);
        for method in &self.methods {
            xml.push_str(&method.to_xml());
        }
        for signal in &self.signals {
            xml.push_str(&signal.to_xml());
        }
        for property in &self.properties {
            xml.push_str(&property.to_xml());
        }
        xml.push_str("    </interface>\n");
        xml
    }
}

/// The full introspection document for one object: its interfaces (custom
/// ones the caller adds, plus the two standard ones every object answers)
/// and any child object names nested directly under its path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeIntrospection {
    interfaces: Vec<InterfaceDescriptor>,
    children: Vec<String>,
}

impl NodeIntrospection {
    /// An empty node: no custom interfaces, no children. `to_xml` will
    /// still report the two standard interfaces every object implements.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a custom interface (e.g. `org.freedesktop.impl.portal.Settings`).
    #[must_use]
    pub fn interface(mut self, interface: InterfaceDescriptor) -> Self {
        self.interfaces.push(interface);
        self
    }

    /// Add a child object, named relative to this node's own path (e.g.
    /// `"request"` under `/org/freedesktop/portal/desktop`, not the full
    /// child path) -- the shape `<node name="…"/>` requires per spec.
    #[must_use]
    pub fn child(mut self, name: impl Into<String>) -> Self {
        self.children.push(name.into());
        self
    }

    /// Render the complete introspection XML document, including the
    /// `org.freedesktop.DBus.Introspectable`/`Properties` interfaces every
    /// object answers (prepended ahead of the caller's own interfaces,
    /// which keeps their order deterministic and independent of
    /// registration order) and every added child node.
    #[must_use]
    pub fn to_xml(&self) -> String {
        let mut xml = String::from(
            "<?xml version=\"1.0\"?>\n\
             <!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\"\n \
             \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n\
             <node>\n",
        );
        xml.push_str(&InterfaceDescriptor::introspectable().to_xml());
        xml.push_str(&InterfaceDescriptor::properties().to_xml());
        for interface in &self.interfaces {
            xml.push_str(&interface.to_xml());
        }
        for child in &self.children {
            let _ = writeln!(xml, "  <node name=\"{child}\"/>");
        }
        xml.push_str("</node>\n");
        xml
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_node_still_reports_the_two_standard_interfaces() {
        let xml = NodeIntrospection::new().to_xml();
        assert!(xml.contains("<interface name=\"org.freedesktop.DBus.Introspectable\">"));
        assert!(xml.contains("<interface name=\"org.freedesktop.DBus.Properties\">"));
        assert!(xml.starts_with("<?xml version=\"1.0\"?>"));
        assert!(xml.trim_end().ends_with("</node>"));
    }

    #[test]
    fn a_method_renders_in_args_before_out_args_each_with_direction() {
        let method = MethodDescriptor::new(
            "ReadAll",
            vec![Argument::new("namespaces", "as")],
            vec![Argument::new("value", "a{sa{sv}}")],
        );
        let xml = method.to_xml();
        let in_pos = xml.find("direction=\"in\"").unwrap();
        let out_pos = xml.find("direction=\"out\"").unwrap();
        assert!(in_pos < out_pos);
        assert!(xml.contains("name=\"namespaces\" type=\"as\""));
        assert!(xml.contains("name=\"value\" type=\"a{sa{sv}}\""));
    }

    #[test]
    fn a_signal_with_no_args_is_self_closing() {
        let signal = SignalDescriptor::new("Closed", vec![]);
        assert_eq!(signal.to_xml(), "      <signal name=\"Closed\"/>\n");
    }

    #[test]
    fn a_signal_with_args_omits_the_direction_attribute() {
        let signal = SignalDescriptor::new(
            "SettingChanged",
            vec![
                Argument::new("namespace", "s"),
                Argument::new("key", "s"),
                Argument::new("value", "v"),
            ],
        );
        let xml = signal.to_xml();
        assert!(!xml.contains("direction"));
        assert!(xml.contains("name=\"namespace\" type=\"s\""));
    }

    #[test]
    fn property_access_renders_the_right_attribute_value() {
        let read = PropertyDescriptor::new("Version", "u", PropertyAccess::Read);
        let write = PropertyDescriptor::new("Config", "s", PropertyAccess::Write);
        let read_write = PropertyDescriptor::new("Volume", "d", PropertyAccess::ReadWrite);

        assert!(read.to_xml().contains("access=\"read\""));
        assert!(write.to_xml().contains("access=\"write\""));
        assert!(read_write.to_xml().contains("access=\"readwrite\""));
    }

    #[test]
    fn an_interface_lists_methods_then_signals_then_properties() {
        let interface = InterfaceDescriptor::new("org.freedesktop.impl.portal.Settings")
            .method(MethodDescriptor::new(
                "ReadAll",
                vec![Argument::new("namespaces", "as")],
                vec![Argument::new("value", "a{sa{sv}}")],
            ))
            .signal(SignalDescriptor::new(
                "SettingChanged",
                vec![Argument::new("namespace", "s")],
            ))
            .property(PropertyDescriptor::new(
                "version",
                "u",
                PropertyAccess::Read,
            ));

        let xml = interface.to_xml();
        let method_pos = xml.find("<method").unwrap();
        let signal_pos = xml.find("<signal").unwrap();
        let property_pos = xml.find("<property").unwrap();
        assert!(method_pos < signal_pos);
        assert!(signal_pos < property_pos);
        assert!(xml.starts_with("    <interface name=\"org.freedesktop.impl.portal.Settings\">"));
        assert!(xml.trim_end().ends_with("</interface>"));
    }

    #[test]
    fn a_node_lists_its_children_after_every_interface() {
        let xml = NodeIntrospection::new()
            .interface(InterfaceDescriptor::new(
                "org.freedesktop.impl.portal.Requests",
            ))
            .child("1_request_token")
            .child("2_request_token")
            .to_xml();

        let last_interface = xml.rfind("</interface>").unwrap();
        let first_child = xml.find("<node name=").unwrap();
        assert!(last_interface < first_child);
        assert!(xml.contains("<node name=\"1_request_token\"/>"));
        assert!(xml.contains("<node name=\"2_request_token\"/>"));
    }

    #[test]
    fn the_standard_introspectable_interface_declares_introspect() {
        let xml = InterfaceDescriptor::introspectable().to_xml();
        assert!(xml.contains("<method name=\"Introspect\">"));
        assert!(xml.contains("name=\"xml_data\" type=\"s\" direction=\"out\""));
    }

    #[test]
    fn the_standard_properties_interface_declares_get_set_getall_and_the_signal() {
        let xml = InterfaceDescriptor::properties().to_xml();
        assert!(xml.contains("<method name=\"Get\">"));
        assert!(xml.contains("<method name=\"Set\">"));
        assert!(xml.contains("<method name=\"GetAll\">"));
        assert!(xml.contains("<signal name=\"PropertiesChanged\">"));
        assert!(xml.contains("name=\"changed_properties\" type=\"a{sv}\""));
    }
}
