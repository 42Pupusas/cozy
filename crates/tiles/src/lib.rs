//! The spiral tile presentation of a directory, shared by the file manager and
//! the portal file chooser.

pub mod dir_sizes;
pub mod fade;
pub mod spiral;
pub mod spiral_cursor;
pub mod tile_coloring;
pub mod tile_field;
pub mod tile_growth;
pub mod tile_id;
pub mod tile_rank;

#[cfg(test)]
mod test_support;
