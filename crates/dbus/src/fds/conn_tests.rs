//! Descriptor passing through `DbusConn`'s real framing path.
//!
//! The sibling `tests` module proves the `sendmsg`/`recvmsg` seam in
//! isolation. These prove the part that was broken: that a descriptor
//! survives the connection's own two-phase frame read (prelude, then body),
//! and lands on the [`Reply`](crate::message::Reply) that declared it.
//!
//! Two `DbusConn`s over a socket pair stand in for client and bus. Neither
//! ran SASL, so negotiation is set directly.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};

use super::fd_frame::FdFrame;
use super::marked_file::MarkedFile;
use super::test_fds::{FdTestGuard, LoanedFds};
use super::SocketPair;
use crate::conn::{DbusConn, DbusError};

/// A client and a peer joined by a real socket pair.
struct Linked {
    client: DbusConn,
    peer: DbusConn,
}

impl Linked {
    fn new() -> Self {
        Self::with_negotiation(true)
    }

    fn with_negotiation(negotiated: bool) -> Self {
        let pair = SocketPair::new().expect("socketpair");
        Self {
            client: DbusConn::over_socket(pair.left, negotiated).expect("client"),
            peer: DbusConn::over_socket(pair.right, negotiated).expect("peer"),
        }
    }
}

fn read_through(fd: &OwnedFd) -> String {
    MarkedFile::read_through(fd)
}

/// The regression this work exists for. The connection read frames with a
/// plain `recv(2)`, which makes the kernel discard `SCM_RIGHTS` silently:
/// the bytes arrived, the descriptors did not, and `unix_fds` reported a
/// count no caller could ever obtain.
#[test]
fn a_descriptor_survives_the_connections_own_frame_read() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("framed");
    let file = marked.open();
    let mut linked = Linked::new();

    let frame = FdFrame::signal_with_fds(1, 1, 1);
    linked
        .peer
        .write_frame(&frame, &[file.as_fd()])
        .expect("peer sends");

    let reply = linked.client.read_frame().expect("client reads");
    assert_eq!(reply.unix_fds, 1, "the header declares one descriptor");
    assert_eq!(reply.fds.len(), 1, "and one must actually have arrived");
    assert_eq!(read_through(&reply.fds[0]), "framed");
}

/// A frame is read in two receives (prelude, then the rest). The kernel
/// attaches ancillary data to whichever byte chunk carried it, so the
/// descriptors arrive with the *first* receive while the count that
/// describes them is only parsed after the second.
#[test]
fn descriptors_arriving_with_the_prelude_reach_the_parsed_message() {
    let _guard = FdTestGuard::acquire();
    let markers = ["one", "two", "three"];
    let files: Vec<std::fs::File> = markers
        .iter()
        .map(|m| MarkedFile::new(m))
        .collect::<Vec<_>>()
        .iter()
        .map(MarkedFile::open)
        .collect();
    let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
    let mut linked = Linked::new();

    let frame = FdFrame::signal_with_fds(7, 3, 3);
    linked
        .peer
        .write_frame(&frame, &borrowed)
        .expect("peer sends");

    let reply = linked.client.read_frame().expect("client reads");
    let read: Vec<String> = reply.fds.iter().map(read_through).collect();
    assert_eq!(read, markers, "h indexes are positional");
    assert_eq!(linked.client.staged_unix_fds(), 0, "nothing left staged");
}

/// The `h` value in a body is an index into the message's descriptor array,
/// not a descriptor number. Resolving it must go through the message.
#[test]
fn a_body_index_resolves_to_the_descriptor_it_names() {
    let _guard = FdTestGuard::acquire();
    let first = MarkedFile::new("idx-first");
    let second = MarkedFile::new("idx-second");
    let (a, b) = (first.open(), second.open());
    let mut linked = Linked::new();

    let frame = FdFrame::signal_with_fds(3, 2, 2);
    linked
        .peer
        .write_frame(&frame, &[a.as_fd(), b.as_fd()])
        .expect("peer sends");

    let reply = linked.client.read_frame().expect("client reads");
    let values = reply.values();
    assert_eq!(values.len(), 2, "two h values in the body");

    let crate::value::Value::UnixFd(index) = values[1] else {
        panic!("the body must decode as unix fd indexes");
    };
    let resolved = reply.fd_at(index).expect("index names a descriptor");
    assert_eq!(read_through(resolved), "idx-second");
}

/// Ordering across messages is the whole contract of the staging queue.
/// Two frames sent back to back must not pool their descriptors.
#[test]
fn two_framed_messages_keep_their_own_descriptors() {
    let _guard = FdTestGuard::acquire();
    let first = MarkedFile::new("msg-a");
    let second = MarkedFile::new("msg-b");
    let (a, b) = (first.open(), second.open());
    let mut linked = Linked::new();

    linked
        .peer
        .write_frame(&FdFrame::signal_with_fds(1, 1, 1), &[a.as_fd()])
        .expect("first");
    linked
        .peer
        .write_frame(&FdFrame::signal_with_fds(2, 1, 1), &[b.as_fd()])
        .expect("second");

    let one = linked.client.read_frame().expect("read first");
    assert_eq!(read_through(&one.fds[0]), "msg-a");
    let two = linked.client.read_frame().expect("read second");
    assert_eq!(read_through(&two.fds[0]), "msg-b");
}

/// A message declaring more descriptors than arrived must fail rather than
/// hand back a short array whose `h` indexes point past the end.
#[test]
fn a_frame_declaring_more_descriptors_than_arrived_is_refused() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("short");
    let file = marked.open();
    let mut linked = Linked::new();

    let frame = FdFrame::signal_with_fds(1, 2, 2);
    linked
        .peer
        .write_frame(&frame, &[file.as_fd()])
        .expect("peer sends one for a declared two");

    let err = linked
        .client
        .read_frame()
        .expect_err("a short descriptor array must not parse");
    assert!(matches!(err, DbusError::FdsTruncated));
}

/// A frame carrying no descriptors must still read normally: every message
/// this workspace sends today is of that shape, and all of them now travel
/// through `recvmsg`.
#[test]
fn an_ordinary_frame_still_reads_with_no_descriptors() {
    let _guard = FdTestGuard::acquire();
    let mut linked = Linked::new();
    let frame = FdFrame::signal_with_fds(11, 0, 0);
    linked.peer.write_frame(&frame, &[]).expect("peer sends");

    let reply = linked.client.read_frame().expect("client reads");
    assert_eq!(reply.unix_fds, 0);
    assert!(reply.fds.is_empty());
    assert_eq!(reply.member.as_deref(), Some("Handles"));
}

/// Silent degradation is refused at the connection layer too: attaching a
/// descriptor to a connection whose peer refused negotiation is an error,
/// not a quietly dropped file.
#[test]
fn sending_descriptors_on_an_unnegotiated_connection_is_refused() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("nonneg");
    let file = marked.open();
    let mut linked = Linked::with_negotiation(false);

    let frame = FdFrame::signal_with_fds(1, 1, 1);
    let err = linked
        .peer
        .write_frame(&frame, &[file.as_fd()])
        .expect_err("must refuse");
    assert!(matches!(err, DbusError::FdsNotNegotiated));
}

/// Descriptors must not outlive the message that owns them.
#[test]
fn descriptors_close_when_the_message_drops() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("drop");
    let files: Vec<std::fs::File> = (0..3).map(|_| marked.open()).collect();
    let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
    let mut linked = Linked::new();

    linked
        .peer
        .write_frame(&FdFrame::signal_with_fds(1, 3, 3), &borrowed)
        .expect("peer sends");

    let reply = linked.client.read_frame().expect("client reads");
    let raw: Vec<i32> = reply.fds.iter().map(AsRawFd::as_raw_fd).collect();
    for fd in &raw {
        assert!(LoanedFds::is_open(*fd));
    }
    drop(reply);
    for fd in raw {
        assert!(!LoanedFds::is_open(fd), "fd {fd} outlived its message");
    }
}

/// The leak probe, at connection level: many frames, many descriptors, and
/// the process's open-descriptor count back where it started.
#[test]
fn repeated_framed_transfers_leak_nothing() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("leak");
    let baseline = LoanedFds::process_open_count();

    for serial in 1..=20u32 {
        let files: Vec<std::fs::File> = (0..4).map(|_| marked.open()).collect();
        let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
        let mut linked = Linked::new();
        linked
            .peer
            .write_frame(&FdFrame::signal_with_fds(serial, 4, 4), &borrowed)
            .expect("send");
        let reply = linked.client.read_frame().expect("read");
        assert_eq!(reply.fds.len(), 4);
    }

    assert_eq!(
        LoanedFds::process_open_count(),
        baseline,
        "every descriptor, socket, and ring must be closed again"
    );
}
