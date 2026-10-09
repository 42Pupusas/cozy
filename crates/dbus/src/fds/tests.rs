//! Transport tests over a real `AF_UNIX` socket pair.
//!
//! Every test here moves real descriptors through the kernel. Nothing is
//! mocked: a passed descriptor is proved by reading through it, and a closed
//! one is proved by asking the kernel.

use std::io::Read as _;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};

use ququmatz::IoUring;

use super::test_fds::{FdTestGuard, LoanedFds};
use super::{FdError, FdPassing, SocketPair};
use crate::sys::DbusSys;

/// One end of a socket pair with its own ring, wrapping the argument
/// shuffling every transport test would otherwise repeat.
struct Endpoint {
    ring: IoUring,
    fd: ququmatz::types::RawFd,
    passing: FdPassing,
    _sock: OwnedFd,
}

impl Endpoint {
    fn new(sock: OwnedFd, negotiated: bool) -> Self {
        let fd = DbusSys::as_uring_fd(&sock);
        let ring = IoUring::builder(8)
            .clamp()
            .cq_entries(16)
            .build()
            .expect("io_uring must be available");
        let mut passing = FdPassing::new();
        passing.set_negotiated(negotiated);
        Self {
            ring,
            fd,
            passing,
            _sock: sock,
        }
    }

    fn send(
        &mut self,
        bytes: &[u8],
        fds: &[std::os::fd::BorrowedFd<'_>],
    ) -> Result<usize, FdError> {
        self.passing
            .send_with_fds(&mut self.ring, self.fd, bytes, fds)
    }

    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, FdError> {
        self.passing.recv_with_fds(&mut self.ring, self.fd, buf)
    }
}

/// A negotiated pair of endpoints, the starting point for most tests.
///
/// Deliberately does not take [`FdTestGuard`]: several tests build more than
/// one `Wired`, and the guard is a plain mutex, so acquiring it here would
/// deadlock the moment a test held it too. Every test in this module takes
/// the guard once, at the top, instead.
struct Wired {
    sender: Endpoint,
    receiver: Endpoint,
}

impl Wired {
    fn new() -> Self {
        let pair = SocketPair::new().expect("socketpair");
        Self {
            sender: Endpoint::new(pair.left, true),
            receiver: Endpoint::new(pair.right, true),
        }
    }
}

/// Read the bytes a descriptor points at, to prove it is the *same* open
/// file the sender had rather than merely some valid descriptor number.
fn read_through(fd: &OwnedFd) -> String {
    use std::io::Seek as _;
    let mut file = std::fs::File::from(fd.try_clone().expect("dup for read"));
    file.rewind().expect("rewind");
    let mut text = String::new();
    file.read_to_string(&mut text).expect("read");
    text
}

/// A temp file with known contents, so a passed descriptor can be identified
/// by what reading it returns.
struct MarkedFile {
    path: std::path::PathBuf,
}

impl MarkedFile {
    fn new(marker: &str) -> Self {
        use std::io::Write as _;
        let path = std::env::temp_dir().join(format!("cozy-fd-{}-{marker}", std::process::id()));
        let mut file = std::fs::File::create(&path).expect("create temp file");
        file.write_all(marker.as_bytes()).expect("write marker");
        Self { path }
    }

    fn open(&self) -> std::fs::File {
        std::fs::File::open(&self.path).expect("reopen temp file")
    }
}

impl Drop for MarkedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn a_message_with_no_descriptors_still_carries_its_bytes() {
    let _guard = FdTestGuard::acquire();
    let mut wired = Wired::new();
    let sent = wired.sender.send(b"plain", &[]).expect("send");
    assert_eq!(sent, 5);

    let mut buf = [0u8; 16];
    let got = wired.receiver.recv(&mut buf).expect("recv");
    assert_eq!(&buf[..got], b"plain");
    assert_eq!(wired.receiver.passing.staged_len(), 0);
}

#[test]
fn one_descriptor_crosses_the_socket_and_reads_back_the_same_file() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("single");
    let file = marked.open();
    let mut wired = Wired::new();

    wired.sender.send(b"x", &[file.as_fd()]).expect("send");
    let mut buf = [0u8; 8];
    wired.receiver.recv(&mut buf).expect("recv");

    let claimed = wired.receiver.passing.claim(1).expect("one descriptor");
    assert_eq!(claimed.len(), 1);
    assert_eq!(read_through(&claimed[0]), "single");
}

/// PLAN.md requires several descriptors in one message. The `h` index is
/// positional, so order must survive the crossing exactly.
#[test]
fn several_descriptors_in_one_message_arrive_in_order() {
    let _guard = FdTestGuard::acquire();
    let markers = ["alpha", "beta", "gamma", "delta"];
    let files: Vec<std::fs::File> = markers
        .iter()
        .map(|m| MarkedFile::new(m))
        .collect::<Vec<_>>()
        .iter()
        .map(MarkedFile::open)
        .collect();
    let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();

    let mut wired = Wired::new();
    wired.sender.send(b"multi", &borrowed).expect("send");
    let mut buf = [0u8; 16];
    wired.receiver.recv(&mut buf).expect("recv");

    let claimed = wired
        .receiver
        .passing
        .claim(markers.len())
        .expect("all four");
    let read: Vec<String> = claimed.iter().map(read_through).collect();
    assert_eq!(read, markers, "h indexes are positional");
}

#[test]
fn a_full_message_of_sixteen_descriptors_is_not_truncated() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("bulk");
    let files: Vec<std::fs::File> = (0..FdPassing::MAX_FDS_PER_MESSAGE)
        .map(|_| marked.open())
        .collect();
    let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();

    let mut wired = Wired::new();
    wired.sender.send(b"bulk", &borrowed).expect("send");
    let mut buf = [0u8; 16];
    wired.receiver.recv(&mut buf).expect("recv");

    assert_eq!(
        wired.receiver.passing.staged_len(),
        FdPassing::MAX_FDS_PER_MESSAGE,
        "the reserved control region must hold a legal maximum message"
    );
}

/// Descriptors from separate messages must not merge into one pool that a
/// single claim could drain out of order.
#[test]
fn descriptors_from_two_messages_are_claimed_per_message() {
    let _guard = FdTestGuard::acquire();
    let first = MarkedFile::new("first");
    let second = MarkedFile::new("second");
    let (a, b) = (first.open(), second.open());

    let mut wired = Wired::new();
    wired.sender.send(b"1", &[a.as_fd()]).expect("send one");
    wired.sender.send(b"2", &[b.as_fd()]).expect("send two");

    let mut buf = [0u8; 8];
    wired.receiver.recv(&mut buf).expect("recv one");
    let one = wired.receiver.passing.claim(1).expect("first descriptor");
    assert_eq!(read_through(&one[0]), "first");

    wired.receiver.recv(&mut buf).expect("recv two");
    let two = wired.receiver.passing.claim(1).expect("second descriptor");
    assert_eq!(read_through(&two[0]), "second");
}

#[test]
fn claiming_more_than_arrived_yields_nothing_and_keeps_the_queue() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("short");
    let file = marked.open();
    let mut wired = Wired::new();
    wired.sender.send(b"s", &[file.as_fd()]).expect("send");
    let mut buf = [0u8; 8];
    wired.receiver.recv(&mut buf).expect("recv");

    assert!(wired.receiver.passing.claim(2).is_none());
    assert_eq!(wired.receiver.passing.staged_len(), 1);
    assert!(wired.receiver.passing.claim(1).is_some());
}

/// Silent degradation is the failure mode this refuses: a caller that
/// attaches descriptors to an unnegotiated connection must hear about it,
/// not wait forever for a file that was dropped on the floor.
#[test]
fn sending_descriptors_without_negotiation_is_refused() {
    let _guard = FdTestGuard::acquire();
    let pair = SocketPair::new().expect("socketpair");
    let mut sender = Endpoint::new(pair.left, false);
    let _receiver = Endpoint::new(pair.right, true);
    let marked = MarkedFile::new("refused");
    let file = marked.open();

    let err = sender
        .send(b"x", &[file.as_fd()])
        .expect_err("must refuse to drop the descriptor silently");
    assert!(matches!(err, FdError::NotNegotiated));
}

#[test]
fn plain_bytes_still_send_on_an_unnegotiated_connection() {
    let _guard = FdTestGuard::acquire();
    let pair = SocketPair::new().expect("socketpair");
    let mut sender = Endpoint::new(pair.left, false);
    let mut receiver = Endpoint::new(pair.right, false);

    sender.send(b"bytes", &[]).expect("no descriptors, no bar");
    let mut buf = [0u8; 8];
    let got = receiver.recv(&mut buf).expect("recv");
    assert_eq!(&buf[..got], b"bytes");
}

#[test]
fn a_closed_peer_reports_closure_rather_than_a_zero_length_read() {
    let _guard = FdTestGuard::acquire();
    let pair = SocketPair::new().expect("socketpair");
    let mut receiver = Endpoint::new(pair.right, true);
    drop(pair.left);

    let mut buf = [0u8; 8];
    let err = receiver.recv(&mut buf).expect_err("peer is gone");
    assert!(matches!(err, FdError::Closed));
}

#[test]
fn received_descriptors_are_close_on_exec() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("cloexec");
    let file = marked.open();
    let mut wired = Wired::new();
    wired.sender.send(b"c", &[file.as_fd()]).expect("send");
    let mut buf = [0u8; 8];
    wired.receiver.recv(&mut buf).expect("recv");

    let claimed = wired.receiver.passing.claim(1).expect("one descriptor");
    assert!(
        LoanedFds::is_cloexec(claimed[0].as_raw_fd()),
        "MSG_CMSG_CLOEXEC must mark received descriptors close-on-exec"
    );
}

// --- descriptor lifetime (PLAN.md Track 1 step 2) ------------------------

#[test]
fn claimed_descriptors_close_when_the_caller_drops_them() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("lifetime");
    let file = marked.open();
    let mut wired = Wired::new();
    wired.sender.send(b"l", &[file.as_fd()]).expect("send");
    let mut buf = [0u8; 8];
    wired.receiver.recv(&mut buf).expect("recv");

    let claimed = wired.receiver.passing.claim(1).expect("one descriptor");
    let raw = claimed[0].as_raw_fd();
    assert!(LoanedFds::is_open(raw));
    drop(claimed);
    assert!(!LoanedFds::is_open(raw), "a dropped descriptor must close");
}

#[test]
fn discarding_staged_descriptors_closes_them() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("discard");
    let files: Vec<std::fs::File> = (0..3).map(|_| marked.open()).collect();
    let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();

    let mut wired = Wired::new();
    wired.sender.send(b"d", &borrowed).expect("send");
    let mut buf = [0u8; 8];
    wired.receiver.recv(&mut buf).expect("recv");

    let staged_raw: Vec<_> = {
        let claimed = wired.receiver.passing.claim(3).expect("three");
        let raw: Vec<_> = claimed.iter().map(AsRawFd::as_raw_fd).collect();
        wired.receiver.passing.discard_staged();
        drop(claimed);
        raw
    };
    for fd in staged_raw {
        assert!(!LoanedFds::is_open(fd));
    }
}

/// The leak test PLAN.md asks for: pass many descriptors, drop them, and
/// confirm the process's own open-descriptor count returns to where it
/// started. Counting `/proc/self/fd` means a leak anywhere -- staged,
/// claimed, or inside the message types -- shows up here.
#[test]
fn passing_many_descriptors_and_dropping_them_leaks_nothing() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("leak");
    // Taken inside the lock, so no other test's sockets are counted.
    let baseline = LoanedFds::process_open_count();

    for _ in 0..25 {
        let files: Vec<std::fs::File> = (0..8).map(|_| marked.open()).collect();
        let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();

        let mut wired = Wired::new();
        wired.sender.send(b"leak", &borrowed).expect("send");
        let mut buf = [0u8; 8];
        wired.receiver.recv(&mut buf).expect("recv");
        let claimed = wired.receiver.passing.claim(8).expect("eight descriptors");
        assert_eq!(claimed.len(), 8);
    }

    assert_eq!(
        LoanedFds::process_open_count(),
        baseline,
        "every descriptor, socket, and ring must be closed again"
    );
}

/// Cleanup after an error: descriptors that arrived for a message the
/// connection then abandons must not survive the connection.
#[test]
fn abandoning_a_connection_mid_message_closes_what_had_arrived() {
    let _guard = FdTestGuard::acquire();
    let marked = MarkedFile::new("abandon");
    let baseline = LoanedFds::process_open_count();
    let staged_raw;

    {
        let files: Vec<std::fs::File> = (0..4).map(|_| marked.open()).collect();
        let borrowed: Vec<_> = files.iter().map(AsFd::as_fd).collect();
        let mut wired = Wired::new();
        wired.sender.send(b"a", &borrowed).expect("send");
        let mut buf = [0u8; 8];
        wired.receiver.recv(&mut buf).expect("recv");
        assert_eq!(wired.receiver.passing.staged_len(), 4);

        let claimed = wired.receiver.passing.claim(4).expect("four");
        staged_raw = claimed.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>();
        // Deliberately never claimed again: the endpoints and the staged
        // descriptors drop together at the end of this scope.
    }

    for fd in staged_raw {
        assert!(!LoanedFds::is_open(fd), "fd {fd} outlived its connection");
    }
    assert_eq!(LoanedFds::process_open_count(), baseline);
}
