//! [`Store`] -- the canvas document's on-disk life: `$XDG_DATA_HOME/
//! terminal/canvas.json`.
//!
//! Typed JSON through `json-bourne` (no generic `Value` layer), so a
//! malformed file fails loudly at the field it cannot parse instead of
//! silently producing a wrong-but-loadable document.

use std::path::PathBuf;

use json_bourne::{FromJson, ToJson, parse_str, to_string_pretty};

use crate::camera::Camera;
use crate::doc::Doc;
use crate::shape::Shape;

/// The serialized form of one shape. Field-for-field with [`Shape`] but
/// flat and open to future kinds, so an old file always loads into a new
/// binary (unknown kinds are skipped, not fatal).
#[derive(Debug, Clone, PartialEq, FromJson, ToJson)]
struct StoredShape {
    kind: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    text: Option<String>,
}

/// The whole persisted document.
#[derive(Debug, Clone, PartialEq, FromJson, ToJson)]
struct StoredDoc {
    version: u32,
    camera_x: f64,
    camera_y: f64,
    camera_zoom: f64,
    shapes: Vec<StoredShape>,
}

/// The document's current on-disk format version. Bump on any breaking
/// change; older versions stay readable (they only ever miss fields).
const FORMAT_VERSION: u32 = 1;

impl StoredShape {
    fn from_shape(shape: &Shape) -> Self {
        let (kind, x, y, w, h, text) = match shape {
            Shape::Rect { x, y, w, h } => ("rect", *x, *y, *w, *h, None),
            Shape::Ellipse { x, y, w, h } => ("ellipse", *x, *y, *w, *h, None),
            Shape::Arrow { ax, ay, bx, by } => ("arrow", *ax, *ay, *bx - *ax, *by - *ay, None),
            Shape::Note { x, y, w, h, text } => ("note", *x, *y, *w, *h, Some(text.clone())),
        };
        Self {
            kind: kind.to_owned(),
            x,
            y,
            w,
            h,
            text,
        }
    }

    fn into_shape(self) -> Option<Shape> {
        match self.kind.as_str() {
            "rect" => Some(Shape::Rect {
                x: self.x,
                y: self.y,
                w: self.w,
                h: self.h,
            }),
            "ellipse" => Some(Shape::Ellipse {
                x: self.x,
                y: self.y,
                w: self.w,
                h: self.h,
            }),
            "arrow" => Some(Shape::Arrow {
                ax: self.x,
                ay: self.y,
                bx: self.x + self.w,
                by: self.y + self.h,
            }),
            "note" => Some(Shape::Note {
                x: self.x,
                y: self.y,
                w: self.w,
                h: self.h,
                text: self.text.unwrap_or_default(),
            }),
            _ => None,
        }
    }
}

/// Load-from / save-to the canvas's data file.
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// The standard storage location.
    #[must_use]
    pub fn path() -> PathBuf {
        let base = std::env::var("XDG_DATA_HOME").map_or_else(
            |_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
                PathBuf::from(home).join(".local").join("share")
            },
            PathBuf::from,
        );
        base.join("terminal").join("canvas.json")
    }

    /// A store bound to the standard location.
    #[must_use]
    pub fn new() -> Self {
        Self { path: Self::path() }
    }

    /// A store bound to an explicit path (tests).
    #[must_use]
    #[cfg(test)]
    pub const fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Where this store reads and writes.
    #[must_use]
    #[cfg(test)]
    pub fn location(&self) -> &std::path::Path {
        &self.path
    }

    /// Load the document and camera. A missing file is the first-run case
    /// and yields an empty document, not an error.
    pub fn load(&self) -> Result<(Doc, Camera), String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Doc::default(), Camera::default()));
            }
            Err(e) => return Err(format!("could not read {}: {e}", self.path.display())),
        };
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{} is not UTF-8", self.path.display()))?;
        let stored: StoredDoc = parse_str(&text)
            .map_err(|e| format!("could not parse {}: {e}", self.path.display()))?;
        let shapes = stored
            .shapes
            .into_iter()
            .filter_map(StoredShape::into_shape)
            .collect();
        let doc = Doc::with_shapes(shapes);
        let camera = Camera::restored((stored.camera_x, stored.camera_y), stored.camera_zoom);
        Ok((doc, camera))
    }

    /// Persist the document and camera, atomically (write a temp file,
    /// rename over the old one) so a crash mid-save cannot corrupt the
    /// only copy.
    pub fn save(&self, doc: &Doc, camera: &Camera) -> Result<(), String> {
        let stored = StoredDoc {
            version: FORMAT_VERSION,
            camera_x: camera.offset().0,
            camera_y: camera.offset().1,
            camera_zoom: camera.zoom(),
            shapes: doc.shapes().iter().map(StoredShape::from_shape).collect(),
        };
        let json = to_string_pretty(&stored)
            .map_err(|e| format!("could not serialize the document: {e}"))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)
            .map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("could not replace {}: {e}", self.path.display()))?;
        Ok(())
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(x: f64, y: f64, text: &str) -> Shape {
        Shape::Note {
            x,
            y,
            w: 100.0,
            h: 40.0,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_saved_document_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("canvas-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = Store::at(dir.join("doc.json"));

        let mut doc = Doc::default();
        doc.begin().push(Shape::Rect {
            x: 1.0,
            y: 2.0,
            w: 30.0,
            h: 20.0,
        });
        doc.begin().push(note(5.0, 5.0, "round trip"));
        doc.begin().push(Shape::Arrow {
            ax: 0.0,
            ay: 0.0,
            bx: 9.0,
            by: 12.0,
        });
        let mut camera = Camera::default();
        camera.pan_by(11, -7);
        camera.zoom_at((0, 0), 1.25);

        store.save(&doc, &camera).expect("save");
        let (loaded_doc, loaded_camera) = store.load().expect("load");

        assert_eq!(loaded_doc.shapes(), doc.shapes());
        assert!((loaded_camera.zoom() - camera.zoom()).abs() < 1e-9);
        assert!((loaded_camera.offset().0 - camera.offset().0).abs() < 1e-9);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_is_an_empty_document() {
        let store = Store::at(std::env::temp_dir().join("canvas-missing-987654.json"));
        let (doc, camera) = store.load().expect("missing file must not error");
        assert!(doc.is_empty());
        assert_eq!(camera, Camera::default());
    }

    #[test]
    fn a_corrupt_file_is_a_reported_error_not_a_silent_reset() {
        let dir = std::env::temp_dir().join(format!("canvas-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = Store::at(dir.join("doc.json"));
        std::fs::write(store.location(), "not json at all").expect("write");
        let err = store.load().expect_err("corrupt file must error");
        assert!(err.contains("parse"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_shape_kind_is_skipped_not_fatal() {
        let dir = std::env::temp_dir().join(format!("canvas-future-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let store = Store::at(dir.join("doc.json"));
        let json = r#"{
            "version": 1,
            "camera_x": 0.0,
            "camera_y": 0.0,
            "camera_zoom": 1.0,
            "shapes": [
                {"kind": "hologram", "x": 1.0, "y": 1.0, "w": 1.0, "h": 1.0, "text": null},
                {"kind": "rect", "x": 5.0, "y": 5.0, "w": 10.0, "h": 10.0, "text": null}
            ]
        }"#;
        std::fs::write(store.location(), json).expect("write");
        let (doc, _) = store.load().expect("load");
        assert_eq!(doc.len(), 1, "the unknown kind was skipped");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn arrow_geometry_round_trips_exactly() {
        // Arrows store (origin + extent), not both endpoints; make sure
        // negative extents survive the trip.
        let stored = StoredShape::from_shape(&Shape::Arrow {
            ax: 10.0,
            ay: 10.0,
            bx: -5.0,
            by: 30.0,
        });
        let back = stored.into_shape().expect("arrow");
        assert_eq!(
            back,
            Shape::Arrow {
                ax: 10.0,
                ay: 10.0,
                bx: -5.0,
                by: 30.0
            }
        );
    }
}
