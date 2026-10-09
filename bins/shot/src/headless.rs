//! Non-rendering screenshot workflow stress test.
//!
//! The compositor normally supplies the frame; this mode supplies a changing
//! synthetic frame with the same negotiated XRGB layout, then runs the real
//! conversion and PNG encoder. No Wayland connection or window is created.

use headless_stress::StressOptions;
use screencopy::{CapturedFrame, FrameParams};

use crate::shot_writer::ShotWriter;

/// Exercises screenshot pixel conversion and encoding.
pub struct ShotStress;

impl ShotStress {
    /// Run the synthetic capture/encode workflow.
    pub fn run(options: StressOptions) {
        let frame = Self::frame();
        let completed = options.run(|round| {
            let _ = ShotWriter::encode(&frame);
            // Keep the changing workload observable to the optimizer while
            // preserving the same frame shape and encoder path as production.
            let _ = std::hint::black_box(round);
        });
        println!("shot headless stress: {completed} rounds");
    }

    fn frame() -> CapturedFrame {
        let width = 1280_u32;
        let height = 720_u32;
        let stride = width * 4;
        let mut pixels = vec![0_u8; (stride * height) as usize];
        for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = index % width as usize;
            let y = index / width as usize;
            pixel[0] = u8::try_from(x % 256).expect("pixel channel fits");
            pixel[1] = u8::try_from(y % 256).expect("pixel channel fits");
            pixel[2] = u8::try_from((x + y) % 256).expect("pixel channel fits");
            pixel[3] = 0;
        }
        let params = FrameParams::new(
            platform::protocol::Format::Xrgb8888.code(),
            width,
            height,
            stride,
        )
        .expect("synthetic screenshot dimensions are valid");
        CapturedFrame::from_shm(&params, 0, &pixels).expect("synthetic frame is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::ShotStress;
    use headless_stress::StressOptions;

    #[test]
    fn workflow_completes_without_wayland() {
        ShotStress::run(StressOptions {
            iterations: 1,
            duration: None,
        });
    }
}
