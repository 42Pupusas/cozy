use std::path::PathBuf;

use crate::shot_archive::ShotArchive;
use crate::shot_options::ShotOptions;
use crate::shot_writer::ShotWriter;

pub struct PngDelivery<'a> {
    png: &'a [u8],
    options: &'a ShotOptions,
}

impl<'a> PngDelivery<'a> {
    #[must_use]
    pub const fn new(png: &'a [u8], options: &'a ShotOptions) -> Self {
        Self { png, options }
    }

    /// Archives and writes everything except the clipboard, returning the path
    /// the clipboard should advertise as its `text/uri-list` target.
    ///
    /// Archiving happens first and always: clipboard publishing parks the
    /// process for as long as it owns the selection, so anything sequenced
    /// after it would not happen until the user copied something else.
    pub fn persist(&self) -> Result<Option<PathBuf>, String> {
        let archived = self.options.save.then(|| self.archive()).transpose()?;
        let wrote_by_other_means = archived.is_some() && self.options.output_path.is_none();
        if !self.options.clipboard && !wrote_by_other_means {
            ShotWriter::write(self.png, self.options.output_path.as_deref())?;
        }
        Ok(self.options.output_path.clone().or(archived))
    }

    fn archive(&self) -> Result<PathBuf, String> {
        let now = localtime::LocalTime::now()
            .ok_or_else(|| "cannot read the local time to name the file".to_string())?;
        let path = ShotArchive::from_environment()?.reserve(&now)?;
        ShotWriter::write(self.png, Some(&path))?;
        println!("{}", path.display());
        Ok(path)
    }
}
