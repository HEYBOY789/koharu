# Downloading and processing projects

Source previews and download/import tasks use a source admission queue. One
source task runs at a time, independently of the existing inference queue.
Downloading a new manga can therefore continue while another project is being
summarized or translated.

When an import has automatic summary or translation enabled, it retains the
imported project, changes the same Activity job to **Queued**, releases source
admission, and waits for inference admission. The next download can start during
that wait. Summary and translation use the existing chapter workflow, including
resume behavior and wiki updates; there is no second translation implementation.

Manual **Translate**, **Summarize**, **Run from here**, and canvas processing
requests also join the inference queue. Requests wait in admission order, with
one project's workflow running at a time. A new request for a project already
reserved by a queued or running workflow is rejected. Use **Run from here** to
schedule the remaining chapters together.

Queued workflows hold a `ProjectLease` for their exact session. Opening or
closing another project does not retarget the work. Workspace leases reserve
projects; `Processing` owns inference admission. A queued canvas request reads a
fresh snapshot of its retained project when admitted.

**Stop** on a queued job cancels only that request. Stopping or finishing the
active workflow releases inference admission for the next request. Import and
inference cancellation use the existing Activity job IDs and stop tokens.

The queues belong to the running application. Closing Koharu does not preserve
pending requests; saved project results remain available for resume.
