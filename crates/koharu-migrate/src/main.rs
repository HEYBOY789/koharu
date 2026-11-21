use std::path::PathBuf;

use anyhow::{Context, Result, ensure};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let source = args
        .next()
        .context("provide the source .khrproj directory")?;
    let destination = args.next().context("provide the destination .khr file")?;
    ensure!(
        args.next().is_none(),
        "usage: koharu-migrate <source.khrproj> <destination.khr>"
    );
    let report = koharu_migrate::convert(&source, &destination).await?;
    println!(
        "Converted {} pages, {} chapters and {} text layers",
        report.pages, report.chapters, report.text_layers
    );
    Ok(())
}
