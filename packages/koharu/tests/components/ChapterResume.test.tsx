import { QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

import { ChapterPanel } from '@/components/editor/ChapterPanel'
import { queryClient } from '@/lib/queries'
import { commands } from '@koharu/bridge/protocol'

afterEach(cleanup)

function setup() {
  vi.spyOn(commands, 'getPage').mockResolvedValue({
    id: 'page',
    label: '001',
    chapter: 'chapter-2',
    size: { width: 10, height: 10 },
    layers: [],
    regions: [],
  })
  vi.spyOn(commands, 'getChapters').mockResolvedValue(
    [1, 2, 3].map((index) => ({
      id: `chapter-${index}`,
      title: `Chapter ${index}`,
      pages: ['page'],
      summarize: true,
      has_summary: true,
      has_setting: true,
    })),
  )
  vi.spyOn(commands, 'getChapter').mockResolvedValue({
    id: 'chapter-2',
    title: 'Chapter 2',
    pages: ['page'],
    summary: 'Saved summary',
    setting: 'Saved setting',
    wiki: { files: [{ slug: 'characters', body: 'Saved wiki' }] },
    summarize: true,
  })
  const run = vi.spyOn(commands, 'runChapters').mockResolvedValue('job')
  render(
    <QueryClientProvider client={queryClient}>
      <ChapterPanel />
    </QueryClientProvider>,
  )
  return run
}

it('resumes from the current chapter by default and supports an explicit rerun', async () => {
  const run = setup()
  const open = await screen.findByRole('button', { name: /Run from here/ })
  fireEvent.click(open)
  const resume = screen.getByRole('switch', { name: 'Resume unfinished work' })
  expect(resume).toHaveAttribute('aria-checked', 'true')
  fireEvent.click(screen.getByRole('button', { name: 'Start' }))
  await waitFor(() =>
    expect(run).toHaveBeenLastCalledWith(['chapter-2', 'chapter-3'], null, true, true),
  )
  await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())

  fireEvent.click(open)
  fireEvent.click(screen.getByRole('switch', { name: 'Resume unfinished work' }))
  fireEvent.click(screen.getByRole('button', { name: 'Start' }))
  await waitFor(() =>
    expect(run).toHaveBeenLastCalledWith(['chapter-2', 'chapter-3'], null, true, false),
  )
  await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
  fireEvent.click(open)
  expect(screen.getByRole('switch', { name: 'Resume unfinished work' })).toHaveAttribute(
    'aria-checked',
    'true',
  )
})

it('keeps a single-chapter Translate action as an explicit rerun', async () => {
  const run = setup()
  fireEvent.click(await screen.findByRole('button', { name: /^Translate$/ }))
  await waitFor(() => expect(run).toHaveBeenCalledWith(['chapter-2'], false, true, false))
})
