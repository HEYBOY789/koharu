---
name: manga-summarizer
description: Summarize exactly one manga chapter, evolve the series wiki, and write the translation setting for that chapter. Use before translating a chapter so names, pronouns, continuity, and scene context stay consistent.
---

# Manga chapter summarizer

You process **one chapter per run**. The chapter images and the current wiki are
already in the workspace. Never look ahead to another chapter and never
summarize the whole volume at once.

Workspace layout, relative to the working directory:

- `pages/` — this chapter's images in reading order, plus `pages/manifest.md`.
- `wiki/` — current series knowledge. Read every file in it before reading the
  images. A new series starts with `world.md`, `characters.md`, `glossary.md`,
  `timeline.md`, and `style.md`, and the folder may hold more categories than
  those five.
- `chapters/chapter.md` — write the chapter summary here.
- `setting.md` — write the translation system prompt here.
- `AGENTS.md` — the same contract in short form.

## 1. Load continuity

Read every file in `wiki/` completely, not just the default ones. Note who is
alive, injured, dead, disguised, or missing; established forms of address;
and world rules. The wiki is the source of truth for previous chapters.

`wiki/style.md` is the source of truth for **how the series is translated**:
voice register per character, sentence length, profanity level, whether
honorifics stay, how sound effects are handled, and which terms get localized
instead of transliterated. This chapter's `setting.md` must agree with it, and
may only tighten it — never silently contradict it.

## 2. Read the chapter images

Work through `pages/` in order, in batches of 3–5 pages. Record:

- Setting: room, building, weather, time, weapons, technology.
- Characters: appearance, expression, actions, injuries, transformations,
  deaths, and who is present on each page.
- Dialogue that matters and the exact original forms of address.
- Twists, reveals, and the final cliffhanger.
- Visual-only facts that dialogue does not state.

If a page is unreadable, say so in the summary instead of inventing content.

## 3. Write `chapters/chapter.md`

Use this exact six-part structure:

```markdown
# [Series] — Chapter [number or folder name]

- **Pages**: [count]
- **Source**: pages/

---

## 1. Setting and world rules in this chapter
- Concrete locations.
- New rules, technology, factions, or powers revealed.

---

## 2. Character beats and relationships
- Characters present and their notable actions.
- New characters: appearance and introduction.
- Changes to health, body, or life status.

---

## 3. Address forms and terminology
### A. Address forms
| Speaker | Listener | Original | Target-language proposal | Nuance and context |
|---|---|---|---|---|

### B. New terms and titles
- **Term**: meaning and nuance.

---

## 4. Detailed chronology
### Segment 1: [title] (pages X–Y)
Describe key dialogue, expressions, and interactions.
...

---

## 5. Canon facts vs inferences vs unknowns
- **Canon facts**: directly drawn or stated.
- **Inferences**: reasoned but unconfirmed.
- **Unknowns**: open questions.

---

## 6. Climax, twist, and cliffhanger
- The turn of the chapter and what carries into the next one.
```

Write in the target translation language named in the task prompt. Quote original
Japanese/Chinese/Korean dialogue only where the wording matters, and always give
the proposed rendering in that language. Keep page ranges accurate.

## 4. Write `setting.md`

`setting.md` is **not** a summary. It is a compact system prompt that the
translation model receives for this chapter. Include:

- Series premise and this chapter's scene in two or three sentences.
- Each speaking character: full name, role, personality, current condition,
  and voice register (rough/cute/formal/archaic/…).
- The exact address-form pairs from section 3, with the target-language choice.
- Glossary terms and names that must stay fixed, and terms to localize.
- Ambiguities: who is speaking, off-page referents, honorific nuance.
- Style rules: sentence length, profanity level, SFX handling, keep honorifics
  or not, do not translate sound effects as dialogue.
- Warnings against continuity mistakes established by the wiki.

Every style rule here must come from `wiki/style.md`, narrowed to this chapter.
When this chapter forces a convention the wiki does not record yet, add it there
in section 5. Prefer short labeled bullet lines over prose paragraphs. Never
include translated dialogue itself, only rules and context.

## 5. Evolve the wiki

After both files exist, update the wiki with whole-file rewrites. Preserve all
existing content and append only what this chapter establishes.

- `characters.md` — add newcomers with appearance, personality, and role. Tag
  every changed status: `[ALIVE]`, `[INJURED — details, chapter]`,
  `[DEAD — chapter, killer, body outcome]`, `[TRANSFORMED]`.
- `world.md` — locations, factions, social rules, powers.
- `glossary.md` — every new address pair and term with its fixed target-language
  rendering.
- `timeline.md` — 3–5 bullet points for this chapter's core events.
- `style.md` — the translation conventions the series now follows. Add a rule
  only when this chapter establishes or forces one: a character's register
  shift, a decision to keep or drop an honorific, a profanity level, an SFX
  convention, a term that must be localized. Keep it short and imperative; it is
  read by every later chapter.

A long series outgrows those files. When this chapter establishes a body of
knowledge that no existing file can hold without burying its own topic — a
faction roster, a power or magic system, a relationship map, a running mystery —
create one new file instead of stretching an existing one:

- File name: lowercase letters, digits, or underscores, ending in `.md`
  (`factions.md`, `power_system.md`).
- Start it with a `# Heading`, then keep the same append-only discipline.
- One topic per file. Do not split a topic that already has a home.
- Never rename, delete, or reorder the default files.

## 6. Finish

Confirm that `chapters/chapter.md` and `setting.md` exist and are non-empty.
Report the three most important continuity facts in your final message. Do not
edit files outside the workspace and do not touch `pages/`.
