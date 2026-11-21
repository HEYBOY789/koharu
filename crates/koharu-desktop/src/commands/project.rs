//! The projects folder and the open project.

use std::sync::Arc;

use anyhow::Context as _;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;

use super::Result;
use crate::app::App;

#[tauri::command]
#[specta::specta]
pub fn list_projects(app: State<'_, Arc<App>>) -> Result<Vec<String>> {
    Ok(app.library.list()?)
}

/// Creates a project and opens it.
#[tauri::command]
#[specta::specta]
pub async fn create(app: State<'_, Arc<App>>, name: String) -> Result<()> {
    let app = Arc::clone(&app);
    Ok(tokio::task::spawn_blocking(move || {
        let session = app.library.create(&name)?;
        app.open(name, session)
    })
    .await??)
}

#[tauri::command]
#[specta::specta]
pub async fn open(app: State<'_, Arc<App>>, name: String) -> Result<()> {
    let app = Arc::clone(&app);
    Ok(tokio::task::spawn_blocking(move || {
        let session = app.library.open(&name)?;
        app.open(name, session)
    })
    .await??)
}

/// Converts a selected project directory and opens the new file.
#[tauri::command]
#[specta::specta]
pub async fn migrate_project(app: State<'_, Arc<App>>, window: Window<CefRuntime>) -> Result<()> {
    let mut dialog = rfd::AsyncFileDialog::new().set_parent(&window);
    if let Some(documents) = dirs::document_dir() {
        dialog = dialog.set_directory(documents.join("Koharu"));
    }
    let Some(folder) = dialog.pick_folder().await else {
        return Ok(());
    };
    let source = folder.path().to_owned();
    let name = source
        .file_stem()
        .and_then(|name| name.to_str())
        .context("the project folder needs a name")?
        .to_owned();
    let destination = app.library.path(&name)?;
    let app = Arc::clone(&app);
    let runtime = tokio::runtime::Handle::current();
    Ok(tokio::task::spawn_blocking(move || {
        runtime.block_on(koharu_migrate::convert(&source, &destination))?;
        let session = app.library.open(&name)?;
        app.open(name, session)
    })
    .await??)
}

/// Deletes a project, closing it first if it's the open one.
#[tauri::command]
#[specta::specta]
pub fn delete_project(app: State<'_, Arc<App>>, name: String) -> Result<()> {
    if app
        .project
        .lock()
        .as_ref()
        .is_some_and(|project| project.name == name)
    {
        app.close();
    }
    Ok(app.library.delete(&name)?)
}

#[tauri::command]
#[specta::specta]
pub fn close(app: State<'_, Arc<App>>) {
    app.close();
}

/// The open project's name, for an interface that loaded after it opened.
#[tauri::command]
#[specta::specta]
pub fn get_open_project(app: State<'_, Arc<App>>) -> Option<String> {
    app.project
        .lock()
        .as_ref()
        .map(|project| project.name.clone())
}
