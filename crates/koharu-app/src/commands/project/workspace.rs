use anyhow::{Context as _, Result, ensure};
use koharu_scene::Snapshot;
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard};
use uuid::Uuid;

use super::{Project, ProjectLibrary};

#[derive(Clone)]
pub(crate) struct JobProject {
    pub(crate) id: Uuid,
    pub(crate) name: String,
}

/// The editor and the single pipeline job may own different open sessions.
/// All access shares one lock so switching cannot race a visible canvas commit.
#[derive(Default)]
pub(crate) struct ProjectWorkspace {
    state: Mutex<Workspace>,
}

#[derive(Default)]
struct Workspace {
    active: Option<Project>,
    background: Option<Project>,
    running: Option<Uuid>,
}

impl ProjectWorkspace {
    pub(crate) async fn lock(&self) -> MappedMutexGuard<'_, Option<Project>> {
        MutexGuard::map(self.state.lock().await, |state| &mut state.active)
    }

    pub(crate) async fn bind(&self, expected: Uuid) -> Result<JobProject> {
        let mut state = self.state.lock().await;
        ensure!(
            state.running.is_none(),
            "another process is already running"
        );
        let project = state.active.as_ref().context("no project is open")?;
        ensure!(
            project.instance_id == expected,
            "the project changed before processing started; start again from the intended project"
        );
        let target = JobProject {
            id: expected,
            name: project.name.clone(),
        };
        state.running = Some(expected);
        Ok(target)
    }

    pub(crate) async fn bind_background(&self, project: Project) -> Result<JobProject> {
        let mut state = self.state.lock().await;
        ensure!(
            state.running.is_none() && state.background.is_none(),
            "another project is already running"
        );
        ensure!(
            !state
                .active
                .as_ref()
                .is_some_and(|active| active.path == project.path),
            "close the imported project before running it in the background"
        );
        let target = JobProject {
            id: project.instance_id,
            name: project.name.clone(),
        };
        state.running = Some(target.id);
        state.background = Some(project);
        Ok(target)
    }

    pub(crate) async fn target(
        &self,
        target: &JobProject,
    ) -> Result<(MappedMutexGuard<'_, Project>, bool)> {
        let state = self.state.lock().await;
        let visible = state
            .active
            .as_ref()
            .is_some_and(|project| project.instance_id == target.id);
        let guard = MutexGuard::try_map(state, |state| {
            if visible {
                state.active.as_mut()
            } else {
                state
                    .background
                    .as_mut()
                    .filter(|project| project.instance_id == target.id)
            }
        })
        .map_err(|_| anyhow::anyhow!("the job's project is no longer retained"))?;
        Ok((guard, visible))
    }

    pub(crate) async fn snapshot(&self, target: &JobProject) -> Result<Snapshot> {
        Ok(self.target(target).await?.0.snapshot())
    }

    pub(crate) async fn finish(&self, target: &JobProject) {
        let mut state = self.state.lock().await;
        if state.running == Some(target.id) {
            state.running = None;
            state.background = None;
        }
    }

    pub(crate) async fn has_background_job(&self) -> bool {
        self.state.lock().await.running.is_some()
    }

    pub(crate) async fn replace(&self, opened: Project) {
        self.state.lock().await.select(Some(opened));
    }

    pub(crate) async fn open(&self, library: &ProjectLibrary, name: &str) -> Result<()> {
        let (_, path) = library.resolve(name)?;
        let path = path.canonicalize()?;
        let mut state = self.state.lock().await;
        if state
            .active
            .as_ref()
            .map(|project| project.path.canonicalize())
            .transpose()?
            == Some(path.clone())
        {
            return Ok(());
        }
        let opened = if state
            .background
            .as_ref()
            .map(|project| project.path.canonicalize())
            .transpose()?
            == Some(path)
        {
            state
                .background
                .take()
                .expect("retained project was checked")
        } else {
            library.open(name).await?
        };
        state.select(Some(opened));
        Ok(())
    }

    pub(crate) async fn close(&self) {
        self.state.lock().await.select(None);
    }

    pub(crate) async fn delete(&self, library: &ProjectLibrary, name: &str) -> Result<bool> {
        let (_, path) = library.resolve(name)?;
        let path = path.canonicalize()?;
        let mut state = self.state.lock().await;
        for project in state.active.iter().chain(state.background.iter()) {
            if state.running == Some(project.instance_id) {
                ensure!(
                    project.path.canonicalize()? != path,
                    "stop the running job before deleting its project"
                );
            }
        }
        let active = state
            .active
            .as_ref()
            .map(|project| project.path.canonicalize())
            .transpose()?
            == Some(path);
        if active {
            state.active = None;
        }
        let library = library.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || library.delete(&name)).await??;
        Ok(active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koharu_scene::{At, Chapter, Chapters, PageDraft, Session, Wiki, WikiFile};

    async fn memory(name: &str) -> Project {
        Project::new(Session::memory().await.unwrap(), name.into(), name.into())
    }

    #[tokio::test]
    async fn imported_project_runs_in_background_without_replacing_the_editor() {
        let workspace = ProjectWorkspace::default();
        workspace.replace(memory("Editing").await).await;
        let imported = workspace
            .bind_background(memory("Imported").await)
            .await
            .unwrap();
        assert_eq!(workspace.lock().await.as_ref().unwrap().name, "Editing");
        assert!(!workspace.target(&imported).await.unwrap().1);
        assert_eq!(
            workspace.snapshot(&imported).await.unwrap().pages().count(),
            0
        );
        assert!(
            workspace
                .bind_background(memory("Another").await)
                .await
                .is_err()
        );
        workspace.finish(&imported).await;
        assert_eq!(workspace.lock().await.as_ref().unwrap().name, "Editing");
        assert!(
            workspace
                .bind_background(memory("Next").await)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn background_project_writes_and_undo_stay_with_the_job_owner() {
        let workspace = ProjectWorkspace::default();
        let mut book = memory("B").await;
        let mut chapter = Chapter::new("Chapter 1");
        let chapter_id = chapter.id;
        let mut output_page = None;
        let patch = book
            .snapshot()
            .patch(|edit| {
                let page =
                    edit.add_page(PageDraft::new("B original page", 100.0, 100.0), At::End)?;
                output_page = Some(page);
                chapter.pages.push(page);
                edit.set_project(&Chapters {
                    entries: vec![chapter],
                })
            })
            .unwrap();
        book.commit(patch).await.unwrap();
        let id = book.instance_id;
        workspace.replace(book).await;
        let target = workspace.bind(id).await.unwrap();
        workspace.close().await;
        assert!(workspace.lock().await.is_none());
        workspace.replace(memory("A").await).await;
        let revision = workspace.lock().await.as_ref().unwrap().revision();

        let (mut book, visible) = workspace.target(&target).await.unwrap();
        assert!(
            !visible,
            "background results must never synchronize the editor canvas"
        );
        let output_page = output_page.unwrap();
        let patch = book
            .snapshot()
            .patch(|edit| {
                edit.set_page(
                    output_page,
                    PageDraft::new("B processed page", 100.0, 100.0),
                )
            })
            .unwrap();
        let commit = book.commit_rebased(patch).await.unwrap().unwrap();
        book.record_commit(&commit);
        let commit = book
            .set_chapter_summary(chapter_id, Some("B summary".into()))
            .await
            .unwrap();
        book.record_commit(&commit);
        let commit = book
            .set_chapter_setting(chapter_id, Some("B prompt".into()))
            .await
            .unwrap();
        book.record_commit(&commit);
        let commit = book
            .set_wiki("characters".into(), "B wiki".into())
            .await
            .unwrap();
        book.record_commit(&commit);
        assert!(book.info().can_undo);
        drop(book);

        {
            let mut active = workspace.lock().await;
            let active = active.as_mut().unwrap();
            assert_eq!(active.name, "A");
            assert_eq!(active.revision(), revision);
            active
                .set_wiki("characters".into(), "A edits".into())
                .await
                .unwrap();
        }
        {
            let (mut book, _) = workspace.target(&target).await.unwrap();
            let value = book.snapshot().chapter(chapter_id).unwrap().unwrap();
            assert_eq!(value.summary.as_deref(), Some("B summary"));
            assert_eq!(value.setting.as_deref(), Some("B prompt"));
            assert_eq!(
                book.snapshot()
                    .page(output_page)
                    .unwrap()
                    .page()
                    .unwrap()
                    .label,
                "B processed page"
            );
            assert_eq!(
                book.snapshot()
                    .wiki()
                    .unwrap()
                    .files
                    .iter()
                    .find(|file| file.slug == "characters")
                    .unwrap()
                    .body,
                "B wiki"
            );
            book.undo().await.unwrap();
            assert!(
                book.snapshot()
                    .wiki()
                    .unwrap()
                    .files
                    .iter()
                    .all(|file| file.body.is_empty())
            );
        }
        assert_eq!(
            workspace
                .lock()
                .await
                .as_ref()
                .unwrap()
                .snapshot()
                .wiki()
                .unwrap()
                .files
                .iter()
                .find(|file| file.slug == "characters")
                .unwrap()
                .body,
            "A edits"
        );
        workspace.finish(&target).await;
    }

    #[tokio::test]
    async fn background_project_reopens_the_retained_session_and_persists_on_finish() {
        let root = tempfile::tempdir().unwrap();
        let library = ProjectLibrary {
            root: root.path().to_owned(),
        };
        let workspace = ProjectWorkspace::default();
        let book = library.create("B").await.unwrap();
        let id = book.instance_id;
        workspace.replace(book).await;
        let target = workspace.bind(id).await.unwrap();
        workspace.replace(library.create("A").await.unwrap()).await;
        let active_id = workspace.lock().await.as_ref().unwrap().instance_id;
        assert!(workspace.bind(active_id).await.is_err());
        assert!(workspace.delete(&library, "B").await.is_err());
        #[cfg(windows)]
        assert!(workspace.delete(&library, "b").await.is_err());
        assert!(library.resolve("B").unwrap().1.is_dir());
        {
            let (mut book, visible) = workspace.target(&target).await.unwrap();
            assert!(!visible);
            book.set_wiki("characters".into(), "saved B".into())
                .await
                .unwrap();
        }
        workspace.open(&library, "B").await.unwrap();
        #[cfg(windows)]
        workspace.open(&library, "b").await.unwrap();
        assert!(workspace.target(&target).await.unwrap().1);
        workspace.open(&library, "A").await.unwrap();
        workspace.finish(&target).await;
        assert!(!workspace.has_background_job().await);
        assert!(workspace.target(&target).await.is_err());
        let book = library.open("B").await.unwrap();
        assert_eq!(
            book.snapshot()
                .wiki()
                .unwrap()
                .files
                .iter()
                .find(|file| file.slug == "characters")
                .unwrap()
                .body,
            "saved B"
        );
    }

    #[tokio::test]
    async fn background_project_rejects_stale_binding_and_cross_project_patches() {
        let workspace = ProjectWorkspace::default();
        let book = memory("B").await;
        let snapshot = book.snapshot();
        let origin = book.instance_id;
        workspace.replace(book).await;
        workspace.replace(memory("A").await).await;
        assert!(workspace.bind(origin).await.is_err());
        let mut active = workspace.lock().await;
        let active = active.as_mut().unwrap();
        let revision = active.revision();
        let patch = snapshot
            .patch(|edit| {
                edit.set_project(&Wiki {
                    files: vec![WikiFile {
                        slug: "characters".into(),
                        body: "foreign".into(),
                    }],
                })
            })
            .unwrap();
        assert!(active.commit_rebased(patch).await.is_err());
        assert_eq!(active.revision(), revision);
    }

    #[tokio::test]
    async fn background_project_switch_waits_for_the_visible_commit_guard() {
        let workspace = std::sync::Arc::new(ProjectWorkspace::default());
        let book = memory("B").await;
        let id = book.instance_id;
        workspace.replace(book).await;
        let target = workspace.bind(id).await.unwrap();
        let next = memory("A").await;
        let (guard, visible) = workspace.target(&target).await.unwrap();
        assert!(visible);
        let switching = workspace.clone();
        let switch = tokio::spawn(async move {
            switching.replace(next).await;
        });
        tokio::task::yield_now().await;
        assert!(!switch.is_finished());
        drop(guard);
        switch.await.unwrap();
        assert!(!workspace.target(&target).await.unwrap().1);
        assert_eq!(workspace.lock().await.as_ref().unwrap().name, "A");
    }

    #[tokio::test]
    async fn background_project_never_follows_a_copied_folder_with_the_same_document_id() {
        let root = tempfile::tempdir().unwrap();
        let library = ProjectLibrary {
            root: root.path().to_owned(),
        };
        let book = library.create("B").await.unwrap();
        let instance = book.instance_id;
        let document = book.snapshot().project_id();
        let copied_path = library.resolve("Copy").unwrap().1;
        std::fs::create_dir(&copied_path).unwrap();
        for file in ["state-a.khr", "state-b.khr"] {
            let source = book.path.join(file);
            if source.is_file() {
                std::fs::copy(source, copied_path.join(file)).unwrap();
            }
        }
        let copied = library.open("Copy").await.unwrap();
        assert_eq!(copied.snapshot().project_id(), document);
        assert_ne!(copied.instance_id, instance);
        let workspace = ProjectWorkspace::default();
        workspace.replace(book).await;
        let target = workspace.bind(instance).await.unwrap();
        let revision = copied.revision();
        workspace.replace(copied).await;
        {
            let (mut original, visible) = workspace.target(&target).await.unwrap();
            assert!(!visible);
            original
                .set_wiki("characters".into(), "Original only".into())
                .await
                .unwrap();
        }
        assert_eq!(
            workspace.lock().await.as_ref().unwrap().revision(),
            revision
        );
        workspace.close().await;
        workspace.open(&library, "B").await.unwrap();
        assert!(workspace.target(&target).await.unwrap().1);
        assert_eq!(workspace.lock().await.as_ref().unwrap().name, "B");
        workspace.finish(&target).await;
    }
}

impl Workspace {
    fn select(&mut self, opened: Option<Project>) {
        if let Some(previous) = self.active.take() {
            if self.running == Some(previous.instance_id) {
                debug_assert!(self.background.is_none());
                self.background = Some(previous);
            }
        }
        self.active = opened;
    }
}
