//! Windows composed of sections, one colour each.
//!
//! Each section is a subsurface showing one colour stretched to its rect by a
//! viewport, so a solid area costs the client no pixel memory. Rounded
//! corners are small shared masks, turned into place by a buffer transform.

pub mod corner_mask;
pub mod frame_pacing;
pub mod logical_rect;
pub mod mask_pool;
pub mod quarter;
pub mod rounded_sections;
pub mod scene_globals;
pub mod scene_window;
pub mod section_fill;
pub mod section_state;
pub mod solid_frame;
pub mod solid_pool;
pub mod solid_section;
pub mod surface_tree;

pub use scene_window::SceneWindow;
pub use solid_frame::{SolidFrame, SolidRect};
pub use surface_tree::TreeUpdate;
