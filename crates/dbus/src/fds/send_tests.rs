//! Sending descriptors through `DbusConn`'s real framing path.
//!
//! The receive side is proved in `conn_tests`. These prove the other
//! direction: that a frame this crate *builds* declares `UNIX_FDS`
//! correctly, that its descriptors ride with it, and that a body index
//! resolves on the far side to the file the sender meant.
//!
//! Every transfer is read back through a second `DbusConn`, so the
//! assertion is end-to-end rather than a check of the bytes we just wrote.

use std::os::fd::AsFd;

use super::SocketPair;
use super::marked_file::MarkedFile;
use super::test_fds::{FdTestGuard, LoanedFds};
use crate::conn::{DbusConn, DbusError};
use crate::value::Value;

/// A sender and a receiver joined by a real socket pair.
struct Linked {
    sender: DbusConn,
    receiver: DbusConn,
}

impl Linked {
    fn new() -> Self {
        Self::with_negotiation(true)
    }

    fn with_negotiation(negotiated: bool) -> Self {
        let pair = SocketPair::new().expect("socketpair");
        Self {
            sender: DbusConn::over_socket(pair.left, negotiated).expect("sender"),
            receiver: DbusConn::over_socket(pair.right, negotiated).expect("receiver"),
        }
    }
}

/// The gap this work closes: nothing could build a frame that declares
/// descriptors, so a portal had no way to hand a file out.
#[test]
fn a_descriptor_sent_in_a_reply_arrives_as_the_same_file() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("sent-reply");
    let file = marked.open();
    let mut linked = Linked::new();

    linked
        .sender
        .send_method_return_with_fds("dest", 11, &[Value::UnixFd(0)], "h", &[file.as_fd()])
        .expect("send");

    let reply = linked.receiver.read_frame().expect("read");
    assert_eq!(reply.unix_fds, 1, "the header must declare the descriptor");
    assert_eq!(reply.fds.len(), 1);
    assert_eq!(MarkedFile::read_through(&reply.fds[0]), "sent-reply");
}

/// The index in the body is positional, so the receiver must be able to
/// pick a specific descriptor out by the index the body names.
#[test]
fn body_indexes_resolve_to_the_descriptors_the_sender_paired_them_with() {
    let _guard = FdTestGuard::acquire();
    let first = MarkedFile::new("send-a");
    let second = MarkedFile::new("send-b");
    let (a, b) = (first.open(), second.open());
    let mut linked = Linked::new();

    linked
        .sender
        .send_method_return_with_fds(
            "dest",
            5,
            &[Value::UnixFd(1), Value::UnixFd(0)],
            "hh",
            &[a.as_fd(), b.as_fd()],
        )
        .expect("send");

    let reply = linked.receiver.read_frame().expect("read");
    let values = reply.values();
    let Value::UnixFd(first_index) = values[0] else {
        panic!("expected a unix fd index");
    };
    assert_eq!(
        MarkedFile::read_through(reply.fd_at(first_index).expect("resolves")),
        "send-b",
        "index 1 names the second attached descriptor"
    );
}

/// A method call carries a file *to* a service, the direction a client
/// takes when handing over a document.
#[test]
fn a_method_call_carries_its_descriptor_to_the_peer() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("call-fd");
    let file = marked.open();
    let mut linked = Linked::new();

    // The call blocks for a reply, so only the frame is written here; the
    // receiver reads it without anyone answering.
    linked
        .sender
        .send_signal_with_fds(
            "/org/example",
            "org.example.Fds",
            "Handed",
            &[Value::UnixFd(0)],
            "h",
            &[file.as_fd()],
        )
        .expect("send");

    let reply = linked.receiver.read_frame().expect("read");
    assert_eq!(MarkedFile::read_through(&reply.fds[0]), "call-fd");
}

/// A descriptor nested inside `a{sv}` is the shape a real portal reply
/// takes, and the marshaller must place its index where the receiver looks.
#[test]
fn a_descriptor_nested_in_a_dictionary_still_crosses() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("nested");
    let file = marked.open();
    let mut linked = Linked::new();

    let body = vec![Value::Array(vec![Value::DictEntry(
        Box::new(Value::Str("fd".into())),
        Box::new(Value::Variant(Box::new(Value::UnixFd(0)))),
    )])];

    linked
        .sender
        .send_method_return_with_fds("dest", 9, &body, "a{sv}", &[file.as_fd()])
        .expect("send");

    let reply = linked.receiver.read_frame().expect("read");
    assert_eq!(reply.unix_fds, 1);
    assert_eq!(MarkedFile::read_through(&reply.fds[0]), "nested");
}

/// A reply with no descriptors must not grow a `UNIX_FDS` field, so
/// ordinary traffic keeps the exact bytes it had before this path existed.
#[test]
fn a_reply_without_descriptors_declares_none() {
    let _guard = FdTestGuard::acquire();
    let mut linked = Linked::new();

    linked
        .sender
        .send_method_return_with_fds("dest", 4, &[Value::Str("plain".into())], "s", &[])
        .expect("send");

    let reply = linked.receiver.read_frame().expect("read");
    assert_eq!(reply.unix_fds, 0);
    assert!(reply.fds.is_empty());
    assert_eq!(reply.values(), vec![Value::Str("plain".into())]);
}

/// The defect the index scan exists to catch: a body naming a descriptor
/// that does not travel with it. The peer cannot detect this, so the
/// sender must.
#[test]
fn a_body_naming_a_descriptor_that_is_not_attached_is_refused() {
    let _guard = FdTestGuard::acquire();
    let mut linked = Linked::new();

    let err = linked
        .sender
        .send_method_return_with_fds("dest", 1, &[Value::UnixFd(2)], "h", &[])
        .expect_err("must refuse");

    assert!(
        matches!(
            err,
            DbusError::FdIndexOutOfRange {
                index: 2,
                attached: 0
            }
        ),
        "got {err:?}"
    );
}

/// The same check must see through nesting, or `a{sv}` becomes a hole in
/// it.
#[test]
fn a_dangling_index_inside_a_variant_is_refused() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("one-only");
    let file = marked.open();
    let mut linked = Linked::new();

    let body = vec![Value::Array(vec![Value::DictEntry(
        Box::new(Value::Str("fd".into())),
        Box::new(Value::Variant(Box::new(Value::UnixFd(3)))),
    )])];

    let err = linked
        .sender
        .send_method_return_with_fds("dest", 1, &body, "a{sv}", &[file.as_fd()])
        .expect_err("must refuse");

    assert!(matches!(err, DbusError::FdIndexOutOfRange { index: 3, .. }));
}

/// Sending a descriptor the peer never agreed to receive would drop it
/// silently, leaving the caller waiting for a file that never comes.
#[test]
fn sending_descriptors_without_negotiation_is_refused() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("unnegotiated");
    let file = marked.open();
    let mut linked = Linked::with_negotiation(false);

    let err = linked
        .sender
        .send_method_return_with_fds("dest", 1, &[Value::UnixFd(0)], "h", &[file.as_fd()])
        .expect_err("must refuse");

    assert!(matches!(err, DbusError::FdsNotNegotiated), "got {err:?}");
}

/// An unnegotiated connection still carries ordinary replies.
#[test]
fn an_unnegotiated_connection_still_sends_plain_replies() {
    let _guard = FdTestGuard::acquire();
    let mut linked = Linked::with_negotiation(false);

    linked
        .sender
        .send_method_return_with_fds("dest", 2, &[Value::U32(42)], "u", &[])
        .expect("plain send must still work");

    let reply = linked.receiver.read_frame().expect("read");
    assert_eq!(reply.values(), vec![Value::U32(42)]);
}

/// The spec caps one message at 16 descriptors. Offering more would have
/// the kernel truncate the control block, losing descriptors silently.
#[test]
fn offering_more_descriptors_than_a_message_allows_is_refused() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("many");
    let files: Vec<std::fs::File> = (0..17).map(|_| marked.open()).collect();
    let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
    let mut linked = Linked::new();

    let err = linked
        .sender
        .send_method_return_with_fds("dest", 1, &[Value::UnixFd(0)], "h", &borrowed)
        .expect_err("must refuse");

    assert!(
        matches!(
            err,
            DbusError::TooManyFds {
                offered: 17,
                limit: 16
            }
        ),
        "got {err:?}"
    );
}

/// The full legal maximum must still cross, so the limit rejects only what
/// the spec rejects.
#[test]
fn the_maximum_legal_descriptor_count_crosses_intact() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("full-house");
    let files: Vec<std::fs::File> = (0..16).map(|_| marked.open()).collect();
    let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
    let mut linked = Linked::new();

    linked
        .sender
        .send_method_return_with_fds("dest", 1, &[Value::UnixFd(15)], "h", &borrowed)
        .expect("16 is legal");

    let reply = linked.receiver.read_frame().expect("read");
    assert_eq!(reply.unix_fds, 16);
    assert_eq!(reply.fds.len(), 16);
    assert_eq!(
        MarkedFile::read_through(reply.fd_at(15).expect("resolves")),
        "full-house"
    );
}

/// The sender only borrows descriptors: the kernel duplicates each into the
/// peer, so the caller's own copies must stay usable afterwards.
#[test]
fn sending_a_descriptor_leaves_the_senders_copy_open() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("still-mine");
    let file = marked.open();
    let mut linked = Linked::new();

    linked
        .sender
        .send_method_return_with_fds("dest", 1, &[Value::UnixFd(0)], "h", &[file.as_fd()])
        .expect("send");
    let _ = linked.receiver.read_frame().expect("read");

    let owned = file
        .try_clone()
        .expect("the sender's descriptor stays open");
    assert_eq!(MarkedFile::read_through(&owned.into()), "still-mine");
}

/// Repeated transfers must not accumulate descriptors in either process.
#[test]
fn repeated_sends_leak_nothing() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("leak-send");
    let before = LoanedFds::process_open_count();
    {
        let mut linked = Linked::new();
        for round in 0..12 {
            let file = marked.open();
            linked
                .sender
                .send_method_return_with_fds(
                    "dest",
                    round + 1,
                    &[Value::UnixFd(0)],
                    "h",
                    &[file.as_fd()],
                )
                .expect("send");
            let reply = linked.receiver.read_frame().expect("read");
            assert_eq!(MarkedFile::read_through(&reply.fds[0]), "leak-send");
        }
    }
    assert_eq!(
        LoanedFds::process_open_count(),
        before,
        "every descriptor from every round must be closed"
    );
}
