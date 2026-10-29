//! Direct parser output sinks.
//!
//! [`TerminalSink`] lets the parser deliver recognised terminal actions without
//! first materialising a [`TermEvent`].  The event callback API remains useful
//! for observers and tests, while the direct path keeps the parser-to-grid
//! production pipeline free of an enum construction and a second dispatch on
//! every printable run.

use crate::{C0, CsiAction, DcsCmd, OscCmd, TermEvent};

/// Receives parsed terminal actions synchronously.
///
/// Borrowed OSC and DCS payloads belong to the parser, so implementations must
/// consume them before the method returns. A sink must not re-enter the parser
/// while handling an action.
pub trait TerminalSink {
    /// Receive one printable Unicode character.
    fn print(&mut self, ch: char);
    /// Receive a contiguous ground-state printable ASCII run.
    fn print_run(&mut self, bytes: &[u8]);
    /// Receive a run of decoded printable characters, in order. Equivalent
    /// to calling [`Self::print`] for each.
    fn print_chars(&mut self, chars: &[char]) {
        for &ch in chars {
            self.print(ch);
        }
    }
    /// Receive a C0 control.
    fn control(&mut self, control: C0);
    /// Receive an ESC action.
    fn esc(&mut self, intermediate: Option<u8>, final_byte: u8);
    /// Receive a completed CSI action.
    fn csi(&mut self, action: CsiAction);
    /// Receive a completed OSC command.
    fn osc(&mut self, command: OscCmd<'_>);
    /// Receive a completed DCS command.
    fn dcs(&mut self, command: DcsCmd<'_>);
    /// Receive input the parser deliberately ignores.
    fn ignored(&mut self);
}

/// Adapts the long-standing event callback API to [`TerminalSink`].
pub struct EventCallback<F> {
    callback: F,
}

impl<F> EventCallback<F> {
    pub const fn new(callback: F) -> Self {
        Self { callback }
    }
}

impl<F> TerminalSink for EventCallback<F>
where
    F: for<'a> FnMut(TermEvent<'a>),
{
    fn print(&mut self, ch: char) {
        (self.callback)(TermEvent::Print(ch));
    }
    fn print_run(&mut self, bytes: &[u8]) {
        (self.callback)(TermEvent::PrintRun(bytes));
    }
    fn control(&mut self, control: C0) {
        (self.callback)(TermEvent::Control(control));
    }
    fn esc(&mut self, intermediate: Option<u8>, final_byte: u8) {
        (self.callback)(TermEvent::Esc {
            intermediate,
            final_byte,
        });
    }
    fn csi(&mut self, action: CsiAction) {
        (self.callback)(TermEvent::Csi(action));
    }
    fn osc(&mut self, command: OscCmd<'_>) {
        (self.callback)(TermEvent::Osc(command));
    }
    fn dcs(&mut self, command: DcsCmd<'_>) {
        (self.callback)(TermEvent::Dcs(command));
    }
    fn ignored(&mut self) {
        (self.callback)(TermEvent::Ignored);
    }
}
