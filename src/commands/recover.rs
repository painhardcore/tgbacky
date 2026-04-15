use std::path::PathBuf;
use std::time::Duration;

use crate::config::AppConfig;
use crate::error::Result;
use crate::recovery::{cleanup_stale_temp_files, scan_stale_temp_files};

pub async fn stale_parts(config: &AppConfig, out: Option<PathBuf>, delete: bool) -> Result<()> {
    let root = out.unwrap_or_else(|| config.download_dir.clone());
    let min_age = Duration::from_secs(config.stale_part_min_age_hours.saturating_mul(3_600));
    let scan = scan_stale_temp_files(&root, &config.temp_extension, min_age)?;
    for file in &scan.stale_files {
        println!("{}  age={}s", file.path.display(), file.age.as_secs());
    }

    let summary = cleanup_stale_temp_files(&root, &config.temp_extension, min_age, delete).await?;
    println!(
        "stale partials: {} found, {} removed, {} files scanned, {} unreadable entries under {}",
        summary.stale_found,
        summary.removed,
        summary.scanned_files,
        summary.unreadable_entries,
        summary.root.display()
    );
    Ok(())
}
