# Following manga

Use **Following** beside Projects and MangaDex to keep a local watch list.

- On a MangaDex result, **Follow English** (or the selected reference language)
  saves that title and language. For an automatic reference, the button uses the
  resolved language from the chapter comparison.
- For another reader website, paste its series URL in Following, choose the
  source language, and select **Follow link**. The existing source importer
  surveys the chapters using the saved website recipe; it can use the configured
  agent if the recipe is missing or no longer works. Configure those tools in
  the existing source-import dialog.
- Opening Following checks stale MangaDex entries through the shared API cache
  (15 minutes). Website entries are checked only on request. **Check now** and
  **Check all updates** request a fresh check. These are in-app checks, not a
  scheduled service when Koharu is closed.
- Website surveys share the import/processing queue. Stop an active or queued
  survey in Activity. **Stop after current check** stops scheduling the rest of
  the watch-list checks; it does not cancel the current native operation.
- A successful first check establishes the baseline. Later checks compare
  chapter identities against the baseline, including chapters added below the
  previous highest chapter. MangaDex merges numbered chapters across translation
  groups and respects volume numbering resets. Website chapters use their URLs.
- Badges remain until **Mark seen** acknowledges the current chapter set.
  Opening a link or creating a project does not acknowledge updates.
- Failed checks retain the last successful chapter data and its timestamp.
  No page images are downloaded and no project is created by a follow/check.

The native application saves the list and acknowledged baseline atomically in
`Koharu/following.json` under the operating system's local application-data
directory. On Windows this is normally `%LOCALAPPDATA%\Koharu\following.json`.
It survives restarting Koharu and does not require a MangaDex account. The list
supports up to 1,000 source/language entries; the UI renders 20 entries per page.
