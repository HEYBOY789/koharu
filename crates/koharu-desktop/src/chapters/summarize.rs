use std::{path::Path, process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use koharu_agent::{CliBackend, CliConfig};
use koharu_pipeline::PipelineConfig;
use koharu_scene::{Chapter, Edit, Op, Session};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;

const SKILL: &str = include_str!("manga-summarizer.md");

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(default)]
pub struct SummarizerConfig {
    pub backend: CliBackend,
    pub binary: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub skill: Option<String>,
    pub summarize_default: bool,
}

impl Default for SummarizerConfig {
    fn default() -> Self {
        Self {
            backend: CliBackend::default(),
            binary: None,
            model: None,
            effort: None,
            skill: None,
            summarize_default: true,
        }
    }
}

impl koharu_config::Section for SummarizerConfig {
    const NAME: &'static str = "summarizer";
}

pub(super) async fn run(
    session: &Arc<Mutex<Session>>,
    chapter: Chapter,
    config: &PipelineConfig,
    stop: &CancellationToken,
) -> Result<()> {
    let config_agent = koharu_config::get::<SummarizerConfig>()?;
    let project = session.lock().project();
    let previous = project.metadata().wiki.clone();
    let mut wiki = previous.clone();
    wiki.ensure_defaults();
    let workspace = tempfile::tempdir()?;
    let root = workspace.path();
    for folder in ["pages", "chapters", "wiki"] {
        tokio::fs::create_dir(root.join(folder)).await?;
    }
    let mut manifest = String::new();
    for (index, &page) in chapter.pages.iter().enumerate() {
        if stop.is_cancelled() {
            return Ok(());
        }
        let background = project
            .page(page)?
            .background
            .context("page has no source image")?;
        let session = Arc::clone(session);
        let path = root.join("pages").join(format!("{:03}.jpg", index + 1));
        tokio::task::spawn_blocking(move || -> Result<()> {
            let bytes = session.lock().file(&background.file)?;
            let image = image::load_from_memory(&bytes)?;
            image.thumbnail(1600, 1600).into_rgb8().save(path)?;
            Ok(())
        })
        .await??;
        use std::fmt::Write as _;
        let label = project
            .metadata()
            .page_labels
            .get(&page)
            .map(String::as_str)
            .unwrap_or("");
        writeln!(&mut manifest, "{:03}.jpg {label}", index + 1)?;
    }
    tokio::fs::write(root.join("pages/manifest.md"), &manifest).await?;
    for file in &wiki.files {
        tokio::fs::write(
            root.join("wiki").join(format!("{}.md", file.slug)),
            &file.body,
        )
        .await?;
    }
    let instructions = match config_agent.skill.as_deref() {
        Some(path) if !path.trim().is_empty() => {
            let path = Path::new(path);
            let path = if path.is_dir() {
                path.join("SKILL.md")
            } else {
                path.to_owned()
            };
            document(&path).await?
        }
        _ => SKILL.to_owned(),
    };
    tokio::fs::write(root.join("AGENTS.md"), instructions).await?;
    let prompt = format!(
        "Read AGENTS.md and summarize the chapter named {:?}. Use the page images in pages/manifest.md order. Write chapters/chapter.md, setting.md and the updated wiki files in language {}. Keep the page images unchanged.",
        chapter.title, config.translator.target,
    );
    let agent = CliConfig {
        backend: config_agent.backend,
        binary: config_agent.binary,
        node: None,
        model: config_agent.model,
        effort: config_agent.effort,
        reasoning: None,
    };
    let mut command = agent.command()?;
    let log = root.join("agent.log");
    let output = std::fs::File::create(&log)?;
    command
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);
    match agent.backend {
        CliBackend::Pi => {
            command.args([
                "--print",
                "--mode",
                "text",
                "--no-session",
                "--approve",
                "--tools",
                "read,write,edit,ls,find",
            ]);
            if let Some(effort) = &agent.effort {
                command.arg("--thinking").arg(effort);
            }
        }
        CliBackend::Agy => {
            command.args(["--output-format", "text", "--dangerously-skip-permissions"]);
            if let Some(effort) = &agent.effort {
                command.arg("--effort").arg(effort);
            }
            command.arg("-p");
        }
    }
    if let Some(model) = &agent.model {
        // AGY takes the value after -p. Pi treats a final argument as the prompt.
        if matches!(agent.backend, CliBackend::Agy) {
            command.arg(&prompt).arg("--model").arg(model);
        } else {
            command.arg("--model").arg(model).arg(&prompt);
        }
    } else {
        command.arg(&prompt);
    }
    let mut process = command.spawn().context("start the chapter summarizer")?;
    let status = tokio::select! {
        _ = stop.cancelled() => {
            process.kill().await?;
            return Ok(());
        }
        status = tokio::time::timeout(Duration::from_secs(20 * 60), process.wait()) => {
            status.context("chapter summary timed out")??
        }
    };
    if !status.success() {
        let mut tail = String::new();
        let mut file = tokio::fs::File::open(&log).await?;
        let length = file.metadata().await?.len();
        use tokio::io::AsyncSeekExt as _;
        file.seek(std::io::SeekFrom::Start(length.saturating_sub(2048)))
            .await?;
        let mut bytes = Vec::new();
        file.take(2048).read_to_end(&mut bytes).await?;
        tail.push_str(&String::from_utf8_lossy(&bytes));
        bail!("summary agent exited with {status}: {}", tail.trim());
    }
    let summary = document(&root.join("chapters/chapter.md")).await?;
    let setting = document(&root.join("setting.md")).await?;
    ensure!(
        !summary.trim().is_empty() && !setting.trim().is_empty(),
        "the summary or translation setting is empty"
    );
    let mut files = tokio::fs::read_dir(root.join("wiki")).await?;
    let mut written = false;
    while let Some(file) = files.next_entry().await? {
        let path = file.path();
        if path.extension().is_some_and(|extension| extension == "md") {
            let slug = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .context("invalid wiki filename")?;
            let text = document(&path).await?;
            if !text.trim().is_empty() {
                wiki.upsert(slug, text)?;
                written = true;
            }
        }
    }
    ensure!(written, "the summarizer left no wiki content");

    let mut session = session.lock();
    let current = session.project();
    let mut metadata = current.metadata().clone();
    ensure!(
        metadata.wiki == previous,
        "the wiki changed during summarization"
    );
    ensure!(
        chapter.pages.iter().all(|&page| {
            current
                .page(page)
                .ok()
                .zip(project.page(page).ok())
                .is_some_and(|(after, before)| after.background == before.background)
        }),
        "chapter images changed during summarization"
    );
    let saved = metadata
        .chapters
        .chapter_mut(chapter.id)
        .context("chapter was removed")?;
    ensure!(
        *saved == chapter,
        "the chapter changed during summarization"
    );
    if !stop.is_cancelled() {
        saved.summary = Some(summary);
        saved.setting = Some(setting);
        metadata.wiki = wiki;
        session.apply(Edit {
            label: "chapter.summarize".to_owned(),
            ops: vec![Op::SetProjectMetadata {
                metadata: Box::new(metadata),
            }],
        })?;
    }
    Ok(())
}

async fn document(path: &Path) -> Result<String> {
    const MAX: u64 = 4 * 1024 * 1024;
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .with_context(|| format!("summary agent did not write {}", path.display()))?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX,
        "invalid summary document {}",
        path.display()
    );
    Ok(tokio::fs::read_to_string(path).await?)
}
