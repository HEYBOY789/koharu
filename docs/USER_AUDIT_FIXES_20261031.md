# User audit fixes — 31 October 2026

Scope: the seven findings in the user's `temp/user-audit-2026-10-31/REPORT.md`, against source baseline `785aa76c`. The original report and its evidence are unchanged.

| Finding | Resulting behavior |
| --- | --- |
| Stop feedback | Toolbar, Chapter and Activity share immediate **Stopping…** feedback. Repeat requests are disabled. The UI explains that the current operation is being awaited; only a native terminal event changes the job to Stopped/Finished/Failed. A rejected stop request unlocks the control for retry. |
| Invalid import URL | Manga and optional reader URLs show a linked inline error after blur, with `aria-invalid` and `aria-describedby`. Tool readiness is displayed separately. |
| Page keyboard navigation | Page buttons expose their label, selected state and current page. Up/Down and Home/End move focus across visible pages, including virtualized rows; Enter/Space select through the existing native selection/cache path. Focus alone does not select. |
| Project library | A visible **Project library** action opens the library while keeping the current editor mounted and jobs running. **Return to …** restores the editor; opening/creating a project also returns to editing. |
| Import scope | **Preview chapters** inspects the chosen reader before import. Users can choose all chapters, an inclusive numeric range (including decimals), individual chapters, or **First chapter only**. Individual selection renders at most 50 rows per page. A source/language/tool change invalidates its old preview. The downloader receives a file containing selected chapter URLs and rejects a mismatched selection before fetching images. |
| Import layout | The dialog uses available width up to 768 CSS px and a bounded scrolling body. Preview/import actions remain outside the scroller. Narrow layouts wrap controls rather than hiding the chapter preset. |
| Accessible labels | Appearance's language combobox has a localized accessible name. Resource summaries interpolate already formatted percentages without adding a second `%`. |

New UI text is supplied in all eleven interface locales. The existing native job/cancellation mechanism owns source inspection as an `inspect` phase; no second queue or cancel API was added. A cancelled preview returns no result. Source discovery may use the configured agent; chapter/page downloads still use the saved reader scraper, not the MangaDex image API.

The missing-chapters preset is not part of this change. Single/range/manual chapter selection provides the requested download limit. Download size is explicitly unknown until images are fetched, rather than estimated from chapter count.

## External toolkit contract

The configured publisher toolkit is a separate repository. This workstation uses `F:\web_truyen\scripts\publishing\main.ts`. Its source-import implementation and focused fixtures were updated locally alongside this change; the already dirty website tree was not broadly committed or pushed.

- `download-source --scan-only` writes `preview.json` in the output folder. It contains `version: 1`, canonical `url`, optional `catalogURL`, metadata including `title`, and a chapter array with `url`, `title`, and numeric `number`. No cover/page images are fetched in this mode.
- `download-source --chapters-file <path>` reads a nonempty, unique JSON array of chapter URLs. Every URL must belong to the current source survey. Filtering happens before cover/page downloading. Only these chapter entries appear in the resulting manifest.
- Koharu checks the survey identity and validates the downloaded manifest against the exact requested chapter set before importing it. Passing the selection by file avoids command-line length limits on Windows.

Local dependency files: `scripts/source-import/download.ts`, `scripts/publishing/main.ts`, `tests/source-import.test.ts`, and `tests/source-discovery.test.ts` in the website repository. Another installation needs a toolkit supporting this contract as well as the updated Koharu source.

## Verification

Focused regressions initially reproduced the missing Stop acknowledgment, missing URL error association, missing library action and absent keyboard page control. Passing checks cover cancellation feedback/deduplication/error recovery/terminal events, retained editor state and background work, source preview invalidation, selected decimal chapter ranges, direct/catalog scraper selection, ordering and cached download resumption.

- 23 distinct focused UI checks, three native source-import checks and two scraper fixtures passed; one native manual import test stayed ignored because it writes to the user's project library.
- TypeScript and locale key/placeholder validation passed. The official bridge generator produced the protocol definitions, including the nullable preview result.
- The frontend static export passed after replacing a stale Webpack cache. The initial cache failure was a `WasmHash` context hash error; no dependency/configuration change was needed.
- Screenshot/DOM checks used actual affected React components, CSS and the application's bundled font with an isolated fake native boundary. Checked 1121 × 769 and 360 × 640 layouts with long labels, fixed import actions, inline validation, keyboard focus across virtualized rows, retained draft text, resource labels and the Language combobox. This is component visual verification, not end-to-end acceptance of the native application or its WebGPU canvas. No full E2E suite or live model/download job was run, and no user project was edited.

Local screenshot evidence is under `temp/user-audit-fixes-20261031/`: `01-import-selection-1121.png`, `02-import-range-1121.png`, `03-import-narrow-360.png`, `04-keyboard-navigation.png`, `05-stopping.png`, `06-project-library.png`, `07-appearance-label.png`, and `08-invalid-url.png`. Screenshots and development fixtures remain ignored, outside the product source commit.

Build/install receipts and exact hashes are recorded in the website workspace's `docs/VALIDATION.md` and `.publishing/debug/user-audit-*` logs. Native visual acceptance remains unverified because the Windows capture tool timed out; the CEF debugging preference was left unchanged.
