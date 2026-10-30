import { QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

import { ChapterPanel } from '@/components/editor/ChapterPanel'
import { SummarizerPreferences } from '@/components/preferences/SummarizerPreferences'
import { chaptersKey, queryClient } from '@/lib/queries'
import { commands, type ChapterInfo, type SummarizerConfig } from '@koharu/bridge/protocol'

afterEach(cleanup)

it.each([
  { enabled: true, rejected: false },
  { enabled: false, rejected: false },
  { enabled: true, rejected: true },
])(
  'applies the saved default and refreshes Workflow only after a successful commit: %o',
  async ({ enabled, rejected }) => {
    let flags = [!enabled, enabled, !enabled]
    const config: SummarizerConfig = {
      backend: 'agy',
      binary: null,
      model: null,
      effort: null,
      skill: null,
      summarize_default: enabled,
    }
    vi.spyOn(commands, 'getSummarizerConfig').mockResolvedValue(config)
    vi.spyOn(commands, 'probeSummarizer').mockResolvedValue({
      path: null,
      version: null,
      models: [],
      error: null,
    })
    vi.spyOn(commands, 'getProject').mockResolvedValue({
      name: 'Book',
      revision: 1,
      active_page: 'page',
      can_undo: false,
      can_redo: false,
    })
    vi.spyOn(commands, 'getPage').mockResolvedValue({
      id: 'page',
      label: '001',
      chapter: 'chapter-1',
      size: { width: 10, height: 10 },
      layers: [],
      regions: [],
    })
    vi.spyOn(commands, 'getChapters').mockImplementation(async () =>
      flags.map((summarize, index) => ({
        id: `chapter-${index + 1}`,
        title: `Chapter ${index + 1}`,
        pages: ['page'],
        summarize,
        has_summary: false,
        has_setting: false,
      })),
    )
    vi.spyOn(commands, 'getChapter').mockImplementation(async () => ({
      id: 'chapter-1',
      title: 'Chapter 1',
      pages: ['page'],
      summary: null,
      setting: null,
      wiki: { files: [] },
      summarize: flags[0],
    }))
    const apply = vi
      .spyOn(commands, 'setChapterSummarize')
      .mockImplementation(async (chapter, summarize) => {
        if (rejected) throw new Error('Workflow is running')
        if (chapter === null) flags = flags.map(() => summarize)
        return null
      })
    render(
      <QueryClientProvider client={queryClient}>
        <SummarizerPreferences />
        <ChapterPanel />
      </QueryClientProvider>,
    )
    const toggle = await screen.findByRole('switch')
    expect(toggle.getAttribute('aria-checked')).toBe(String(!enabled))
    const button = await screen.findByRole('button', {
      name: 'Apply to all chapters in this project',
    })
    fireEvent.click(button)
    await waitFor(() => expect(apply).toHaveBeenCalledWith(null, enabled))
    if (rejected) {
      expect(await screen.findByRole('alert')).toHaveTextContent('Workflow is running')
      expect(toggle.getAttribute('aria-checked')).toBe(String(!enabled))
      return
    }
    await waitFor(() => expect(toggle.getAttribute('aria-checked')).toBe(String(enabled)))
    expect(apply).toHaveBeenCalledTimes(1)
    expect(
      queryClient
        .getQueryData<ChapterInfo[]>(chaptersKey)
        ?.every((chapter) => chapter.summarize === enabled),
    ).toBe(true)
  },
)
