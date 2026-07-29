//! Wallpaper decode/scale stress without Wayland or layer-shell.
use headless_stress::StressOptions;
use wallpaper::{ScaleMode, WallpaperImage};

/// Exercises the real PNG decode, scaling, and RGBA-to-XRGB workflow.
pub struct WallpaperStress;

impl WallpaperStress {
    /// Run headless mode when requested.
    pub fn maybe_run(arguments: &[String]) -> Result<bool, String> {
        let Some(options) = StressOptions::parse(arguments.iter().cloned())? else {
            return Ok(false);
        };
        let path = arguments
            .windows(2)
            .find(|pair| pair[0] == "--stress-path")
            .map(|pair| pair[1].clone())
            .ok_or_else(|| "--stress-path is required for wallpaper stress".to_owned())?;
        let mode = Self::requested_mode(arguments)?;
        let bytes = std::fs::read(&path).map_err(|error| format!("{path}: {error}"))?;
        let image =
            WallpaperImage::load(&bytes, 1 << 30).map_err(|error| format!("{path}: {error}"))?;
        let mut rgba = vec![0u8; 1280 * 720 * 4];
        let modes = [
            ScaleMode::Stretch,
            ScaleMode::Fit,
            ScaleMode::Fill,
            ScaleMode::Center,
            ScaleMode::Tile,
        ];
        let completed = options.run(|round| {
            let selected = mode.unwrap_or_else(|| {
                modes[usize::try_from(round % modes.len() as u64).unwrap_or_default()]
            });
            selected.blit_into(&image, 1280, 720, [0, 0, 0, 255], &mut rgba);
            let index =
                usize::try_from(round % u64::try_from(rgba.len()).unwrap_or(1)).unwrap_or_default();
            let _ = std::hint::black_box(rgba[index]);
        });
        println!("wallpaper headless stress: {completed} rounds");
        Ok(true)
    }

    /// Select one scaling mode for isolated profiling, or all modes when the
    /// option is absent so the normal stress workflow remains representative.
    fn requested_mode(arguments: &[String]) -> Result<Option<ScaleMode>, String> {
        let Some(value) = arguments
            .windows(2)
            .find(|pair| pair[0] == "--stress-mode")
            .map(|pair| pair[1].as_str())
        else {
            return Ok(None);
        };
        let mode = match value {
            "stretch" => ScaleMode::Stretch,
            "fit" => ScaleMode::Fit,
            "fill" => ScaleMode::Fill,
            "center" => ScaleMode::Center,
            "tile" => ScaleMode::Tile,
            _ => {
                return Err(format!(
                    "--stress-mode must be stretch, fit, fill, center, or tile; got {value}"
                ));
            }
        };
        Ok(Some(mode))
    }
}
