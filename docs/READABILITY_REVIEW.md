# Editor readability review

The editor agent reviews the native prepared frame used by canvas and exports. Codex, Pi and AGY share the same review tools.

For a normal request such as “Make the lettering readable, fix small text, overlaps and thick outlines”, the agent chooses the requested chapter/page range, audits it, inspects images, repairs selected layers and verifies the result. Start with a small range to review the visual result before asking for a whole project.

## Tools and native guarantees

- `set_edit_scope` with `readability: true` starts a review over the entire inclusive range. The range cannot shrink to hide unchecked pages. Typography/geometry edits also activate the review if an agent omitted the flag.
- `audit_readability` returns actual fitted text sizes at 390px and 960px page widths (never upscaling the page), overflow diagnostics, overlap/duplicate candidates, outline-to-font ratios and local background contrast. Follow pagination. The default 12px display font is a triage threshold, not a universal typography standard.
- Duplicate detection uses overlapping original OCR boxes even after automatic placement separates the rendered copies. It also recognizes a merged OCR box when separate visible detections cover its source words. The audit lists those surviving layers. The agent checks the rendered dialogue before removing a candidate with a different translation.
- `view_page` without crop/width returns phone and desktop images during a review. `element` crops around a layer while retaining surrounding artwork; `display_width` requests a specific simulated width.
- `repair_text` takes `page` and optional `elements`. A page call checks its visible translated layers, up to 50 per call. It reconciles proven merged OCR while keeping the surviving dialogue complete. An unambiguous leading word missing from a generated fragment moves into that fragment before the merged layer is removed. A Vietnamese plural already explicit in the surviving sentence is retained. Other wording differences require visual review. Comparable dialogue in the same balloon receives a common fitted size. The search tries closer internal margins, line spacing and positions inside the contour. It preserves each balloon's geometry and font faces, adjusts outlines and contrast for the background, and saves one undo step.
- Existing typography, geometry and line-break edits preview candidates before commit. They reject disappearing text, text moved beyond the page, new/worse overflow, unreadably smaller text, thicker excessive outlines, and worse uniform-background contrast below the review threshold. When a balloon is balanced, an oversized fragment can decrease to the common size while its smallest dialogue improves. A rejected visual candidate returns feedback for another attempt or an unresolved finding. Invalid tool arguments use the existing bounded recovery ledger. Omitted properties are preserved.
- After edits, view both sizes and use `verify_readability`. Each full-page view refreshes the current measurements. Repeated inspection and edits that leave the page unchanged retain its verification state. Each remaining issue needs a decision. Suspected collision/duplicate false positives can be kept. Measured problems must be repaired or marked `needs_review` with a reason. Native completion checks coverage and verifies that reviewed page state is still current.
- The progress panel shows checked pages and unresolved findings separately from saved edits. Finishing a review with unresolved findings does not mean those problems were repaired.

Readability repair preserves dialogue wording, artwork and cleanup. It cannot remove ordinary dialogue to pass an audit, hide cleanup, or rerun detection/OCR/translation/inpainting. Audited duplicate candidates can be removed after visual inspection. All native revision checks, retained project ownership and Undo remain in effect.

A proposal to delete or hide text without duplicate evidence returns `candidate_rejected` with no mutation. It does not create a pending malformed-call failure that could be cleared only by performing the prohibited edit. The agent must choose a safe layout repair or verify the remaining measured defect as `needs_review`. Invalid arguments, stale state and scope errors keep their bounded recovery checks. Production and the native installed-agent fixture execute the same layer review validation.

## Limits

Bounding rectangles do not prove glyph collisions. Contrast sampling on mixed artwork is advisory and requires crops; it does not certify accessibility. A dense printed page may still require zoom or a revised balloon layout on a phone. Native fitting never expands a rectangle across artwork to reach a font threshold; the agent must inspect and selectively adjust legitimate balloon space, or leave an explicit review finding. Review state belongs to the current agent turn and is not a persistent project audit database.

## Validation

Focused native tests cover real fitted-font diagnostics, device scaling, hidden text, overlap/duplicate candidates, background-aware contrast, regression rejection, coverage, stale page verification and honest unresolved decisions. A detached real-project fixture checks five manga pages, rendered at both widths, and asserts unchanged words, bubble geometry, unrelated typography and artwork/cleanup pixels. It intentionally submits smaller text with a huge outline to confirm rejection before saving. Test images and project copies stay under ignored `temp/`.

The installed-agent integration fixture uses the same scene edits and readability module on a detached two-page range. It is opt-in and requires an available signed-in CLI/model; native tests or a changed-property count alone are not autonomous visual acceptance.

Pi's managed installer launcher and npm JavaScript entry points are resolved through Node with explicit arguments. Editor sessions load Koharu's explicit tool extension and use Pi's `--no-approve` option for project-local resources. The user's configured Pi provider/model identifies the model, for example `local-proxy/ag/gemini-3.8-flash-high`.
