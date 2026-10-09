use crate::ShotApp;
use crate::png_delivery::PngDelivery;
use crate::shot_options::{CaptureTarget, ShotOptions};
use crate::window_selector::WindowSelector;
use crate::window_source::WindowSources;

pub struct WindowCommand;

impl WindowCommand {
    pub fn list() -> Result<(), String> {
        let windows = WindowSources::detect()?.windows()?;
        let listing = WindowSelector::new(&windows).listing();
        if !listing.is_empty() {
            println!("{listing}");
        }
        Ok(())
    }

    pub fn capture(options: &ShotOptions) -> Result<(), String> {
        let CaptureTarget::Window(query) = &options.target else {
            return Err("internal: window capture requested without a window target".to_string());
        };
        let source = WindowSources::detect()?;
        let windows = source.windows()?;
        let window = WindowSelector::new(&windows).select(query)?;
        let png = source.capture_png(window, options.overlay_cursor)?;
        let path = PngDelivery::new(&png, options).persist()?;
        if options.clipboard {
            let wake = waker::Waker::new()
                .map_err(|e| platform::PlatformError::ReaderSpawn(e).to_string())?;
            ShotApp::connect(wake)?.publish_clipboard(&png, path.as_deref())?;
        }
        Ok(())
    }
}
