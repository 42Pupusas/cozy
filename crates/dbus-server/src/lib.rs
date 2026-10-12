//! The D-Bus **server** side: owning a name, routing incoming method calls
//! to registered objects/interfaces, and answering introspection/property
//! queries -- everything [`dbus`] itself doesn't provide beyond the
//! low-level `request_name`/`recv_message`/`send_reply` primitives
//! `statusbar`'s tray watcher already proved out by hand.
//!
//! This is a separate crate rather than a feature-gated module inside
//! `dbus` on purpose: a portal daemon (or any other from-scratch D-Bus
//! service) is the only kind of consumer that ever needs an object
//! registry, a method router, or introspection XML. Every existing
//! consumer of `dbus` (`statusbar`'s `NetworkManager`/bluez/logind
//! wrappers) is purely a client and should never carry this crate's state
//! or dependencies at all -- a `server` cargo feature would still leave
//! every default build's dependency graph *able* to pull this code in by
//! flipping one flag, whereas a separate crate makes "does this binary
//! link the server machinery" a visible, checked-in `Cargo.toml` fact
//! instead of a feature flag someone has to remember exists.
//!
//! Re-exports the [`dbus`] types every interface implementation built on
//! top of this crate will need (`Value`, `Reply`, the marshaller/
//! unmarshaller), so downstream crates (`portal-core`, `bins/portal`) can
//! depend on `dbus-server` alone rather than adding `dbus` as a second,
//! separately versioned dependency just to name a `Value`.

pub mod call_context;
pub mod call_queue;
pub mod dispatcher;
pub mod introspection;
pub mod name_watcher;
pub mod properties;
pub mod registry;
pub mod router;
pub mod serve;
pub mod server_loop;

pub use call_context::CallContext;
pub use call_queue::{CallQueue, PendingCall};
pub use dbus::marshal::Marshaller;
pub use dbus::message::Reply;
pub use dbus::value::{Unmarshaller, Value};
pub use dispatcher::Dispatcher;
pub use introspection::{
    Argument, InterfaceDescriptor, MethodDescriptor, NodeIntrospection, PropertyAccess,
    PropertyDescriptor, SignalDescriptor,
};
pub use name_watcher::NameWatcher;
pub use properties::{PropertiesChangedSignal, PropertyTable};
pub use registry::ObjectRegistry;
pub use router::{CallOutcome, DispatchError, DispatchOutcome, MethodCallRouter};
pub use serve::{IncomingCall, ReplySink};
pub use server_loop::{Received, ServerLoop};
