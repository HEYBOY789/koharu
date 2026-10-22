use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

pub(crate) enum SkillSource {
    Vendored,
    External(PathBuf),
}

impl SkillSource {
    fn read(&self) -> Result<String> {
        match self {
            Self::Vendored => Ok(include_str!("skill/SKILL.md").to_owned()),
            Self::External(path) => std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display())),
        }
    }
}

/// Installs the skill inside the chapter workspace so Pi and Agy discover it
/// relative to the working directory. An external source also brings scripts
/// and references along.
pub(crate) fn install(workspace: &Path, source: &SkillSource) -> Result<PathBuf> {
    let root = workspace
        .join(".agents")
        .join("skills")
        .join("manga-summarizer");
    std::fs::create_dir_all(&root)
        .with_context(|| format!("failed to create {}", root.display()))?;
    match source {
        SkillSource::Vendored => {
            std::fs::write(root.join("SKILL.md"), source.read()?)
                .context("failed to write the summarizer skill")?;
        }
        SkillSource::External(skill) => {
            let directory = skill.parent().unwrap_or_else(|| Path::new("."));
            for entry in walkdir::WalkDir::new(directory)
                .follow_links(false)
                .into_iter()
                .filter_map(|entry| entry.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(directory)
                    .with_context(|| format!("{} is outside the skill", entry.path().display()))?;
                if relative.components().next().is_some_and(|component| {
                    matches!(
                        component.as_os_str().to_str(),
                        Some("node_modules" | ".git")
                    )
                }) {
                    continue;
                }
                let target = root.join(relative);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &target).with_context(|| {
                    format!(
                        "failed to copy {} into the chapter workspace",
                        entry.path().display()
                    )
                })?;
            }
            if !root.join("SKILL.md").is_file() {
                bail!("the selected skill has no SKILL.md");
            }
        }
    }
    Ok(root)
}
