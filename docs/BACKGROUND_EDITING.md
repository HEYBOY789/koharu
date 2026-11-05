# Editing while another project translates

Paint, Eraser and ordinary layer edits in the open project do not wait for another project's translation workflow. Their changes and Undo belong to the edited project.

Remove uses AI inpainting. Its stroke receives priority between background processing steps: the current in-flight stage work finishes and commits, Remove runs, then the background scheduler continues with its remaining pages. An active chapter summarizer must also finish before Remove can run. Remove does not cancel an API request or run a second inference model concurrently.

Ordinary chapter/translation workflows remain queued in project order. Stop on a queued Remove cancels that stroke without stopping the background project. A stroke retains its originating project and page, including when the editor switches to another project while it waits.

The Chapter panel describes work belonging to its own project. Activity continues to show background jobs and their individual Stop controls.

Focused regression tests cover the real pipeline scheduler with controlled stage processors, priority/cancellation, the summarizer's inference reservation, native raster edits/Undo and project switching. These checks do not make a live model call or modify the user's manga projects.
