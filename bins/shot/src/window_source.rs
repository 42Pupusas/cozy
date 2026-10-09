use crate::niri_windows::NiriWindows;
use crate::window_selector::WindowInfo;

pub trait WindowSource {
    fn windows(&self) -> Result<Vec<WindowInfo>, String>;

    fn capture_png(&self, window: &WindowInfo, include_cursor: bool) -> Result<Vec<u8>, String>;
}

pub struct WindowSources;

impl WindowSources {
    pub fn detect() -> Result<Box<dyn WindowSource>, String> {
        if let Some(niri) = NiriWindows::from_environment() {
            return Ok(Box::new(niri));
        }
        Err("no window capture backend for this compositor (supported: niri)".to_string())
    }
}
