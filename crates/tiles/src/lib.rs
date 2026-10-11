//! The spiral tile presentation of a directory, shared by the file manager and
//! the portal file chooser.

pub mod density;
pub mod dir_sizes;
pub mod fade;
pub mod metric_overrides;
pub mod pinned_card;
pub mod row_kind;
pub mod spacing;
pub mod spiral;
pub mod spiral_cursor;
pub mod theme;
pub mod tile_action;
pub mod tile_coloring;
pub mod tile_field;
pub mod tile_geometry;
pub mod tile_growth;
pub mod tile_id;
pub mod tile_rank;
pub mod typography;

#[cfg(test)]
mod test_support;
