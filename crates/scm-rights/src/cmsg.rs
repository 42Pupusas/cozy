//! The `struct cmsghdr` layout and the alignment rule every control-message
//! region obeys, shared by the build side ([`crate::OutgoingMessage`]) and
//! the parse side ([`crate::IncomingControl`]).

#[allow(non_camel_case_types)]
pub type size_t = usize;

pub const SOL_SOCKET: i32 = 1;
pub const SCM_RIGHTS: i32 = 1;

/// `struct cmsghdr` — the header of one ancillary-data item. `#[repr(C)]`
/// only pins layout (order/type/size), not names, so the fields drop the
/// shared `cmsg_` prefix the C struct uses.
#[repr(C)]
pub struct CmsgHdr {
    pub len: size_t,
    pub level: i32,
    pub kind: i32,
}

pub const CMSG_HDR_LEN: usize = std::mem::size_of::<CmsgHdr>();

/// Extension trait rounding a byte length up to the platform word
/// alignment, matching the kernel's `CMSG_ALIGN`.
///
/// Was a free `cmsg_align(len: usize)` function -- its sole argument is a
/// bare `usize`, so it reads as a method on that value
/// (`len.cmsg_aligned()`), matching the `RawSyscallReturn`-on-`isize` style
/// used in `pty::sys` for the same kind of "raw primitive, no natural
/// receiver type" helper.
pub trait CmsgAlign {
    fn cmsg_aligned(self) -> Self;
}

impl CmsgAlign for usize {
    fn cmsg_aligned(self) -> Self {
        let a = std::mem::size_of::<Self>();
        (self + a - 1) & !(a - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmsg_aligned_rounds_up_to_the_word_size() {
        let word = std::mem::size_of::<usize>();
        assert_eq!(0usize.cmsg_aligned(), 0);
        assert_eq!(1usize.cmsg_aligned(), word);
        assert_eq!(word.cmsg_aligned(), word);
        assert_eq!((word + 1).cmsg_aligned(), word * 2);
    }

    #[test]
    fn cmsg_header_matches_the_c_abi_layout() {
        assert_eq!(CMSG_HDR_LEN, std::mem::size_of::<size_t>() + 4 + 4);
    }
}
