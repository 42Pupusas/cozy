//! Scratch probes for the coverage review — verifying suspected edge-case bugs.

use vt_parser::{CsiAction, Event, Parser};

/// `CSI ;5H` — leading empty param should default row=1, col=5.
#[test]
fn csi_leading_empty_param() {
    let mut p = Parser::new();
    let mut got = None;
    p.feed(b"\x1b[;5H", |ev| {
        if let Event::Csi(CsiAction::CursorPosition { row, col }) = ev {
            got = Some((row, col));
        }
    });
    assert_eq!(got, Some((1, 5)), "leading empty param must default to 1");
}

/// OSC with a non-digit byte below b'0' in the Ps field must not panic.
#[test]
fn osc_non_numeric_ps_no_panic() {
    let mut p = Parser::new();
    p.feed(b"\x1b]+\x07", |_| {});
}

/// An ESC arriving mid-CSI must abort the sequence and start a fresh one.
#[test]
fn esc_aborts_partial_csi() {
    let mut p = Parser::new();
    let mut events = Vec::new();
    p.feed(b"\x1b[12\x1b[3A", |ev| {
        if let Event::Csi(a) = ev {
            events.push(a);
        }
    });
    assert_eq!(events, vec![CsiAction::CursorUp(3)]);
}

/// A sequence split across `feed()` calls must reassemble.
#[test]
fn csi_split_across_feeds() {
    let mut p = Parser::new();
    let mut got = None;
    p.feed(b"\x1b[", |_| {});
    p.feed(b"5", |_| {});
    p.feed(b"A", |ev| {
        if let Event::Csi(a) = ev {
            got = Some(a);
        }
    });
    assert_eq!(got, Some(CsiAction::CursorUp(5)));
}

/// UTF-8 surrogate-range encodings (0xED 0xA0 0x80 = D800) must yield U+FFFD.
#[test]
fn utf8_surrogate_is_replaced() {
    let mut p = Parser::new();
    let mut seen = Vec::new();
    p.feed(&[0xED, 0xA0, 0x80], |ev| {
        if let Event::Print(c) = ev {
            seen.push(c);
        }
    });
    assert_eq!(seen, vec!['\u{FFFD}']);
}

/// More than 16 CSI params must not panic (Params fixed cap).
#[test]
fn csi_param_overflow_no_panic() {
    let mut p = Parser::new();
    p.feed(
        b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20m",
        |_| {},
    );
}

/// Param value overflow (u16 saturation) must not panic.
#[test]
fn csi_param_value_saturates() {
    let mut p = Parser::new();
    let mut got = None;
    p.feed(b"\x1b[999999999A", |ev| {
        if let Event::Csi(CsiAction::CursorUp(n)) = ev {
            got = Some(n);
        }
    });
    assert_eq!(got, Some(u16::MAX));
}
