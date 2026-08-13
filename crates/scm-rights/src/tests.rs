use std::os::fd::{AsFd, AsRawFd, BorrowedFd, IntoRawFd, RawFd};

use super::{IncomingControl, OutgoingMessage};
use crate::cmsg::{CMSG_HDR_LEN, CmsgAlign, CmsgHdr, SCM_RIGHTS, SOL_SOCKET};

/// Distinct real descriptors for the tests that parse a control region.
///
/// [`IncomingControl::fds`] *takes ownership* of every descriptor it finds,
/// so a parsed region must name descriptors nothing else owns. Handing it
/// the same borrowed fd twice (stdin, say) produces two owners of one
/// number and aborts the process on the second close. This opens `/dev/null`
/// once per slot and releases ownership immediately, leaving the parsed
/// `OwnedFd`s as the sole owners.
struct LoanedFds {
    raw: Vec<RawFd>,
}

impl LoanedFds {
    fn open(count: usize) -> Self {
        let raw = (0..count)
            .map(|_| {
                std::fs::File::open("/dev/null")
                    .expect("/dev/null must be openable")
                    .into_raw_fd()
            })
            .collect();
        Self { raw }
    }

    fn borrowed(&self) -> Vec<BorrowedFd<'_>> {
        self.raw
            .iter()
            // SAFETY: each fd came from a File this type released ownership
            // of, and stays open until the parsed OwnedFd closes it. The
            // borrow cannot outlive `self`.
            .map(|fd| unsafe { BorrowedFd::borrow_raw(*fd) })
            .collect()
    }
}

/// Write one cmsg header into a fresh buffer, for the malformed-input cases
/// that cannot be produced through [`OutgoingMessage`].
struct HandBuiltControl;

impl HandBuiltControl {
    fn with_header(len: usize, level: i32, kind: i32, payload: usize) -> Vec<u8> {
        let mut buf = vec![0u8; CMSG_HDR_LEN + payload];
        // SAFETY: `buf` is at least CMSG_HDR_LEN long; the pointer is only
        // ever fed to `write_unaligned`, never dereferenced typed.
        unsafe {
            #[allow(clippy::cast_ptr_alignment)]
            let cmsg = buf.as_mut_ptr().cast::<CmsgHdr>();
            cmsg.write_unaligned(CmsgHdr { len, level, kind });
        }
        buf
    }
}

#[test]
fn empty_control_region_yields_no_fds() {
    assert!(IncomingControl::fds(&[]).is_empty());
    assert!(IncomingControl::fds(&[0u8; 4]).is_empty());
}

#[test]
fn one_fd_roundtrips_through_an_outgoing_message() {
    let loaned = LoanedFds::open(1);
    let msg = OutgoingMessage::new(b"hello", &loaned.borrowed());
    let fds = IncomingControl::fds(msg.control_bytes());
    assert_eq!(fds.len(), 1);
}

/// The D-Bus case that motivated the shared crate: a portal reply can carry
/// several descriptors in one message, and every one must come back.
#[test]
fn many_fds_roundtrip_in_a_single_control_region() {
    let loaned = LoanedFds::open(5);
    let borrowed = loaned.borrowed();
    let msg = OutgoingMessage::new(b"payload", &borrowed);
    let fds = IncomingControl::fds(msg.control_bytes());
    assert_eq!(fds.len(), borrowed.len());
    let mut seen: Vec<RawFd> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), borrowed.len(), "each slot is a distinct fd");
}

#[test]
fn no_fds_means_no_control_region_at_all() {
    let mut msg = OutgoingMessage::new(b"bare", &[]);
    assert!(msg.control_bytes().is_empty());
    let hdr = msg.header();
    assert!(hdr.msg_control.is_null());
    assert_eq!(hdr.msg_controllen, 0);
}

#[test]
fn header_points_at_the_control_region_when_fds_are_attached() {
    let stdin = std::io::stdin();
    let mut msg = OutgoingMessage::new(b"hi", &[stdin.as_fd()]);
    let len = msg.control_bytes().len();
    let hdr = msg.header();
    assert!(!hdr.msg_control.is_null());
    assert_eq!(hdr.msg_controllen, len);
    assert_eq!(hdr.msg_iovlen, 1);
}

/// The kernel is free to split descriptors across several `SCM_RIGHTS`
/// items in one control region. Reading only the first header (as an
/// earlier version did) silently leaks every descriptor after it.
#[test]
fn a_second_scm_rights_item_in_the_chain_is_not_missed() {
    let head = LoanedFds::open(1);
    let tail = LoanedFds::open(2);
    let first = OutgoingMessage::new(b"a", &head.borrowed());
    let second = OutgoingMessage::new(b"b", &tail.borrowed());
    let mut chained = first.control_bytes().to_vec();
    chained.extend_from_slice(second.control_bytes());

    let fds = IncomingControl::fds(&chained);
    assert_eq!(fds.len(), 3, "both cmsg items must be walked");
}

#[test]
fn a_truncated_cmsg_len_is_treated_as_absent() {
    let buf = HandBuiltControl::with_header(9999, SOL_SOCKET, SCM_RIGHTS, 0);
    assert!(IncomingControl::fds(&buf).is_empty());
}

#[test]
fn a_cmsg_len_below_the_header_size_is_rejected() {
    let buf = HandBuiltControl::with_header(4, SOL_SOCKET, SCM_RIGHTS, 8);
    assert!(IncomingControl::fds(&buf).is_empty());
}

#[test]
fn a_zero_cmsg_len_cannot_spin_the_walk() {
    let buf = HandBuiltControl::with_header(0, SOL_SOCKET, SCM_RIGHTS, 8);
    assert!(IncomingControl::fds(&buf).is_empty());
}

#[test]
fn a_non_scm_rights_item_is_ignored() {
    let buf = HandBuiltControl::with_header(CMSG_HDR_LEN + 8, SOL_SOCKET, 99, 8);
    assert!(IncomingControl::fds(&buf).is_empty());
}

#[test]
fn a_credentials_item_before_the_rights_item_does_not_hide_it() {
    const SCM_CREDENTIALS: i32 = 2;
    let loaned = LoanedFds::open(1);
    let mut chained =
        HandBuiltControl::with_header(CMSG_HDR_LEN + 8, SOL_SOCKET, SCM_CREDENTIALS, 8);
    let rights = OutgoingMessage::new(b"x", &loaned.borrowed());
    chained.extend_from_slice(rights.control_bytes());

    let fds = IncomingControl::fds(&chained);
    assert_eq!(fds.len(), 1, "the rights item follows an ignored item");
}

#[test]
fn reserve_len_grows_with_the_descriptor_count() {
    let one = IncomingControl::reserve_len(1);
    let many = IncomingControl::reserve_len(16);
    // Alignment can make adjacent fd counts collide on the same rounded
    // word boundary, so compare a small count against a much larger one
    // to guarantee a real difference.
    assert!(many > one);
    // Always at least big enough for the cmsg header itself.
    assert!(one >= CMSG_HDR_LEN);
    assert_eq!(IncomingControl::reserve_len(0), CMSG_HDR_LEN.cmsg_aligned());
}

/// The width reserved for `n` descriptors must actually hold the control
/// region `OutgoingMessage` builds for `n` descriptors, or a receiver sizes
/// its buffer too small and the kernel truncates the ancillary data.
#[test]
fn reserve_len_covers_what_the_send_side_builds() {
    let stdin = std::io::stdin();
    for count in 1usize..=8 {
        let borrowed = vec![stdin.as_fd(); count];
        let msg = OutgoingMessage::new(b"x", &borrowed);
        assert!(
            IncomingControl::reserve_len(count) >= msg.control_bytes().len(),
            "reserve_len({count}) must cover the built control region"
        );
    }
}
