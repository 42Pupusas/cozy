use portal_core::{ChooserDialog, ChooserRequest, DialogFactory};

use crate::chooser_window::ChooserWindow;

pub struct WindowDialogs;

impl DialogFactory for WindowDialogs {
    fn open(&mut self, request: ChooserRequest) -> Result<Box<dyn ChooserDialog>, String> {
        ChooserWindow::open(&request).map(|window| Box::new(window) as Box<dyn ChooserDialog>)
    }
}
