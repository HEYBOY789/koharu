# Readability layout search

## Problem and outcome

The existing repair fits typography inside the current text frame. Pi then guesses
separate geometry and typography edits; it can leave tiny text untouched even when
a taller frame would work. A saved edit or a clean overlap report does not prove
that the page is readable.

Extend `repair_text` with a preview/choose workflow for difficult layers. Pi marks
permitted placement space from the rendered and original artwork. Native code
tries complete layout patches and returns at most three measured previews. Pi
compares the phone and desktop images, applies a candidate, then verifies the
saved page.

## Contract and boundaries

- Keep the existing page/batch repair for ordinary typography and proven duplicates.
- For layout search, use one visible text layer, an explicit permitted rectangle,
  and optional protected rectangles in source-image coordinates. These constrain
  the full text frame and its rendered bounds, including the outline.
- Vary frame width, height and position with fitted font size, fill and outline in
  the same reversible patch. Keep semantic text, reading direction, source OCR,
  original artwork and cleanup unchanged. Slanted text can retain its angle or
  use a level candidate; prefer its original angle when readability is equal.
- Use the native prepared frame and renderer for all measurements and previews.
  Reject overflow, new text overlaps, worse measured readability, and placement
  outside permitted space or over protected areas.
- Search is bounded and read-only. Return up to three distinct candidates with
  phone (390 px) and desktop (960 px) previews, measured sizes and remaining issues.
- Applying a candidate requires the same project/page revision and review scope.
  Re-audit and inspect the saved result before marking the page verified.
- Pi must try this search before declaring cramped text impossible. If no safe
  candidate reaches the reader minimum, report the remaining limitation explicitly.

## Implementation

- [x] Add a focused native regression with cramped text, protected artwork and a
  stale-candidate check; reproduce the current size-only limitation.
- [x] Implement bounded joint geometry/typography search and candidate selection
  in the existing readability owner and `repair_text` tool.
- [x] Update the production host, the existing acceptance host, and embedded agent
  instructions together. Keep candidate previews out of saved project state.
- [x] Run focused checks and Pi on a detached copy of Chapter 8, page 9. Compare
  actual phone/desktop renders and preserve original project state.
- [x] Build/install the Windows executable, review the diff, commit/push source,
  and record measured results and remaining limitations here.

## Acceptance

The regression must demonstrate a larger, fully rendered text size from a joint
patch without crossing protected regions, changing words/artwork or accepting a
stale proposal. For the real page, capture before/after images and report every
remaining tiny/unclear layer honestly. Numerical thresholds support visual review;
they do not replace it.

## Results

The native regression first failed on the missing layout-search contract, then
exposed a rotated frame crossing permitted space despite safe painted bounds.
It now obtains larger phone-readable candidates with both bounds protected,
verifies read-only previews and one-step Undo, and rejects a stale choice after
Undo. Nineteen focused checks passed, including the CLI bridge sending each
preview's label before its image. A small target margin prevents reader scaling
from rounding 12px down to 11.999999px.

Final Pi acceptance used the configured `local-proxy/ag/gemini-3.8-flash-high`
model on a detached copy of Chapter 8, page 9. It performed eight searches and
saved five selected layouts. The text on the hair improved from 6.53px to 12.01px
at 390px page width; the upper-right dialogue and lower narration also reached
12.01px. Two small-text findings remain honestly marked `needs_review` (11.83px
and 10.39px). All six translations and the original project's state hashes are
unchanged. Both 390px and 960px before/after renders were inspected.

The debug Windows executable was built, installed and checked with `--version`;
the previous executable is retained as a backup. Samples, renders and installation
receipts remain in ignored `temp/`, outside source control.
