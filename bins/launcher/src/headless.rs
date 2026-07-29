//! Launcher workflow stress without a layer-shell window or text renderer.
use headless_stress::StressOptions;
use launcher::Catalog;

/// Exercises the production catalog scan and fuzzy ranking path.
pub struct LauncherStress;

impl LauncherStress {
    /// Run headless mode when its flag is present.
    pub fn maybe_run(arguments: &[String]) -> Result<bool, String> {
        let Some(options) = StressOptions::parse(arguments.iter().cloned())? else {
            return Ok(false);
        };
        let catalog = Catalog::load();
        let queries = ["", "term", "file", "browser", "network", "terminal", "x"];
        let completed = options.run(|round| {
            let index = usize::try_from(round % queries.len() as u64).unwrap_or_default();
            let results = catalog.search(queries[index]);
            let _ = std::hint::black_box(results.len());
            if let Some(result) = results.first() {
                let _ = std::hint::black_box(result.entry.name.as_str());
            }
        });
        println!(
            "launcher headless stress: {completed} rounds, {} applications",
            catalog.len()
        );
        Ok(true)
    }
}
