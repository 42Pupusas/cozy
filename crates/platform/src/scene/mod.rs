//! Windows composed of sections: the window surface plus subsurfaces, each
//! showing one colour stretched to its rect by a viewport, so a solid area
//! costs the client no pixel memory.

pub mod frame_pacing;
pub mod logical_rect;
pub mod scene_globals;
pub mod scene_window;
pub mod section_state;
pub mod solid_frame;
pub mod solid_pool;
pub mod solid_section;
pub mod surface_tree;

pub use scene_window::SceneWindow;
pub use solid_frame::{SolidFrame, SolidRect};
pub use surface_tree::TreeUpdate;
