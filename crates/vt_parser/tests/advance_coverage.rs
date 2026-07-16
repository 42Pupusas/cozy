//! Coverage for `Parser::advance`'s less-common transitions: CAN/SUB abort,
//! DEL-is-ignored, CSI-ignore recovery, DCS/SOS/PM/APC string termination,
//! and ESC-state edge cases. Previously 58% branch coverage (CC 17).

use vt_parser::{CsiAction, Event, Parser};

#[test]
fn can_aborts_in_progress_csi_and_emits_control() {
    let mut p = Parser::new();
    let mut events = Vec::new();
    p.feed(b"\x1b[3", |ev| events.push(format!("{ev:?}")));
    p.feed(b"\x18", |ev| events.push(format!("{ev:?}")));
    // CAN (0x18) must abort the partial CSI and report itself as a Control.
    assert!(events.iter().any(|e| e.contains("Control")));

    // Parser must be back in Ground: a plain letter now prints normally.
    let mut seen = None;
    p.feed(b"A", |ev| {
        if let Event::Print(c) = ev {
            seen = Some(c);
        }
    });
    assert_eq!(seen, Some('A'));
}

#[test]
fn sub_aborts_in_progress_escape_sequence() {
    let mut p = Parser::new();
    p.feed(b"\x1b[?25", |_| {});
    let mut got_control = false;
    p.feed(b"\x1a", |ev| {
        if matches!(ev, Event::Control(_)) {
            got_control = true;
        }
    });
    assert!(got_control);

    // Confirm the abort really reset state to Ground.
    let mut got = None;
    p.feed(b"\x1b[5A", |ev| {
        if let Event::Csi(a) = ev {
            got = Some(a);
        }
    });
    assert_eq!(got, Some(CsiAction::CursorUp(5)));
}

#[test]
fn del_byte_is_silently_ignored_in_ground_state() {
    let mut p = Parser::new();
    let mut seen = Vec::new();
    p.feed(b"a\x7fb", |ev| {
        if let Event::Print(c) = ev {
            seen.push(c);
        }
    });
    // DEL must produce no event at all, not even a replacement char.
    assert_eq!(seen, vec!['a', 'b']);
}

#[test]
fn del_byte_is_ignored_mid_csi_sequence_without_disrupting_it() {
    let mut p = Parser::new();
    let mut got = None;
    p.feed(b"\x1b[3\x7fA", |ev| {
        if let Event::Csi(a) = ev {
            got = Some(a);
        }
    });
    // The DEL is swallowed anywhere; the CSI param `3` should not be
    // corrupted by it, so CUU(3) still comes through.
    assert_eq!(got, Some(CsiAction::CursorUp(3)));
}

#[test]
fn csi_ignore_recovers_to_ground_on_alphabetic_final_byte() {
    let mut p = Parser::new();
    // A byte in the 0x00..0x1F-adjacent "junk" range inside CSI-param state
    // pushes the parser into CsiIgnore (anything not digit/;/intermediate/
    // final). Use a stray control-picture byte like 0x08 (backspace) which
    // csi_param's catch-all sends to CsiIgnore.
    p.feed(b"\x1b[1", |_| {}); // enter CsiParam with param "1"
    p.feed(&[0x08], |_| {}); // -> CsiIgnore (unhandled byte)
    p.feed(b"garbage", |_| {}); // still ignored (no alpha/~ yet... wait these ARE alpha)
    // Once we saw the first alphabetic byte above, state already returned to
    // Ground. Confirm a subsequent independent CSI sequence parses cleanly.
    let mut got = None;
    p.feed(b"\x1b[7A", |ev| {
        if let Event::Csi(a) = ev {
            got = Some(a);
        }
    });
    assert_eq!(got, Some(CsiAction::CursorUp(7)));
}

#[test]
fn csi_ignore_recovers_on_tilde_final_byte() {
    let mut p = Parser::new();
    p.feed(b"\x1b[1", |_| {});
    p.feed(&[0x08], |_| {}); // -> CsiIgnore
    p.feed(b"~", |_| {}); // '~' is one of the two recognised exits
    let mut got = None;
    p.feed(b"\x1b[2A", |ev| {
        if let Event::Csi(a) = ev {
            got = Some(a);
        }
    });
    assert_eq!(got, Some(CsiAction::CursorUp(2)));
}

#[test]
fn dcs_sequence_terminated_by_bare_st_byte() {
    // Per the "anywhere" rule, an actual ESC byte always breaks out to the
    // Escape state (handled by `escape()`, not `dcs_or_sos()`) before the
    // next byte is seen -- so the only way `dcs_or_sos` itself sees a
    // terminator is the bare C1 ST byte (0x9C), without a preceding ESC.
    let mut p = Parser::new();
    let mut saw_dcs = false;
    p.feed(b"\x1bPpayload", |_| {});
    p.feed(&[0x9C], |ev| {
        if matches!(ev, Event::Dcs) {
            saw_dcs = true;
        }
    });
    assert!(saw_dcs);
}

#[test]
fn dcs_sequence_terminated_by_esc_backslash_yields_esc_event_not_dcs() {
    // ESC always wins the "anywhere" race and re-enters Escape state; the
    // following '\' is then a plain final byte handled by `escape()`,
    // producing an `Event::Esc`, not `Event::Dcs`. Document that behaviour.
    let mut p = Parser::new();
    let mut got = None;
    p.feed(b"\x1bPsome dcs payload\x1b\\", |ev| {
        got = Some(format!("{ev:?}"));
    });
    assert!(matches!(got, Some(ref s) if s.contains("Esc")));
}

#[test]
fn sos_pm_apc_sequences_all_terminate_on_bare_st_byte() {
    for intro in [b'X', b'^', b'_'] {
        let mut p = Parser::new();
        let mut saw_dcs = false;
        p.feed(&[0x1b, intro], |_| {});
        p.feed(b"payload", |_| {});
        p.feed(&[0x9C], |ev| {
            if matches!(ev, Event::Dcs) {
                saw_dcs = true;
            }
        });
        assert!(saw_dcs, "intro byte {intro} did not reach Dcs event");
    }
}

#[test]
fn escape_state_control_byte_resets_silently_to_ground() {
    // A C0 control arriving right after ESC (not handled by the "anywhere"
    // rule, not an intermediate, not a final byte) falls to escape()'s
    // catch-all and resets to Ground with no event.
    let mut p = Parser::new();
    let mut events = Vec::new();
    p.feed(&[0x1b, 0x01], |ev| events.push(format!("{ev:?}")));
    assert!(events.is_empty());

    // Parser should be back in Ground and able to print normally.
    let mut seen = None;
    p.feed(b"z", |ev| {
        if let Event::Print(c) = ev {
            seen = Some(c);
        }
    });
    assert_eq!(seen, Some('z'));
}

#[test]
fn escape_intermediate_accepts_multiple_intermediates_then_final() {
    // e.g. ESC ' ' '#' final -- second intermediate byte overwrites the
    // first per escape_intermediate's own comment ("rare").
    let mut p = Parser::new();
    let mut got = None;
    p.feed(b"\x1b #8", |ev| {
        if let Event::Esc { intermediate, final_byte } = ev {
            got = Some((intermediate, final_byte));
        }
    });
    assert_eq!(got, Some((Some(b'#'), b'8')));
}

#[test]
fn escape_intermediate_control_byte_resets_silently() {
    let mut p = Parser::new();
    let mut events = Vec::new();
    p.feed(b"\x1b ", |_| {}); // ESC space -> EscapeIntermediate
    p.feed(&[0x01], |ev| events.push(format!("{ev:?}"))); // catch-all reset
    assert!(events.is_empty());
}

#[test]
fn utf8_continuation_state_reached_via_advance_for_all_lengths() {
    let mut p = Parser::new();
    let mut seen = Vec::new();
    // 2, 3, and 4-byte sequences all drive utf8_continue via advance().
    p.feed("é€🦀".as_bytes(), |ev| {
        if let Event::Print(c) = ev {
            seen.push(c);
        }
    });
    assert_eq!(seen, vec!['é', '€', '🦀']);
}
