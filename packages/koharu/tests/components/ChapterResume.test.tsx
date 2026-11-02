import { QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

import { CanvasCommandBar } from '@/components/editor/CanvasCommandBar'
import { ChapterPanel } from '@/components/editor/ChapterPanel'
import { projectKey, queryClient } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands, type Job } from '@koharu/bridge/protocol'

afterEach(cleanup)

function setup() {
  queryClient.setQueryData(projectKey, {
    name: 'Editing',
    revision: 1,
    active_page: 'page',
    can_undo: false,
    can_redo: false,
  })
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
      <CanvasCommandBar />
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

const download: Job = {
  id: 'download',
  project: 'New manga',
  state: 'running',
  completed: 1,
  total: 5,
  page: null,
  chapter: null,
  chapter_title: 'https://reader.example/manga',
  chapter_progress: null,
  phase: 'import',
  stage: null,
  model: null,
  detail: null,
  retry: null,
  error: null,
}

it('allows translation while a different project downloads and keeps the toolbar Stop on inference', async () => {
  useKoharuStore.setState({ jobs: { download }, processingScope: 'page' })
  const run = setup()
  const translate = await screen.findByRole('button', { name: /^Translate$/ })
  expect(translate).toBeEnabled()
  expect(screen.getByRole('button', { name: 'Run processing' })).toBeEnabled()
  expect(screen.queryByRole('button', { name: 'Stop' })).not.toBeInTheDocument()
  fireEvent.click(translate)
  await waitFor(() => expect(run).toHaveBeenCalledWith(['chapter-2'], false, true, false))
  const stop = vi.spyOn(commands, 'stopJob').mockResolvedValue(null)
  act(() =>
    useKoharuStore.setState({
      jobs: {
        download,
        translation: {
          ...download,
          id: 'translation',
          project: 'Editing',
          phase: 'translate',
          chapter: 'chapter-2',
        },
      },
    }),
  )
  expect(translate).toBeDisabled()
  fireEvent.click(screen.getAllByRole('button', { name: 'Stop' }).at(-1)!)
  expect(stop).toHaveBeenCalledWith('translation')
})

it('reserves only the queued import target and ignores a source preview without a project', async () => {
  const run = setup()
  const translate = await screen.findByRole('button', { name: /^Translate$/ })
  act(() =>
    useKoharuStore.setState({
      jobs: {
        preview: { ...download, id: 'preview', project: null, phase: 'inspect' },
        queued: { ...download, id: 'queued', state: 'queued', phase: 'translate' },
      },
    }),
  )
  expect(translate).toBeEnabled()
  act(() =>
    useKoharuStore.setState({
      jobs: {
        queued: {
          ...download,
          id: 'queued',
          project: 'Editing',
          state: 'queued',
          phase: 'translate',
        },
      },
    }),
  )
  expect(translate).toBeDisabled()
  expect(screen.getByText('Queued')).toBeVisible()
  expect(screen.getByRole('button', { name: 'Stop processing' })).toBeEnabled()
  expect(run).not.toHaveBeenCalled()
})

it('queues Run from here behind another project and cancels only the queued request', async () => {
  const other = { ...download, phase: 'translate' as const, chapter_title: 'Chapter 1' }
  useKoharuStore.setState({ jobs: { other }, processingScope: 'page' })
  const run = setup()
  const stop = vi.spyOn(commands, 'stopJob').mockResolvedValue(null)
  const start = await screen.findByRole('button', { name: /Run from here/ })
  expect(start).toBeEnabled()
  expect(screen.getByRole('button', { name: 'Queue processing' })).toBeEnabled()
  run.mockImplementation(async () => {
    useKoharuStore.setState({
      jobs: {
        other,
        queued: {
          ...other,
          id: 'queued-editing',
          project: 'Editing',
          state: 'queued',
          chapter: 'chapter-2',
          chapter_title: 'Chapter 2',
        },
      },
    })
    return 'queued-editing'
  })
  fireEvent.click(start)
  fireEvent.click(screen.getByRole('button', { name: 'Start' }))
  await waitFor(() =>
    expect(run).toHaveBeenCalledWith(['chapter-2', 'chapter-3'], null, true, true),
  )
  await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
  expect(screen.getByText('Queued')).toBeVisible()
  expect(start).toBeDisabled()
  fireEvent.click(screen.getByRole('button', { name: 'Stop processing' }))
  expect(stop).toHaveBeenCalledWith('queued-editing')
  expect(useKoharuStore.getState().jobs.other.state).toBe('running')
})

it('keeps a single-chapter Translate action as an explicit rerun', async () => {
  const run = setup()
  fireEvent.click(await screen.findByRole('button', { name: /^Translate$/ }))
  await waitFor(() => expect(run).toHaveBeenCalledWith(['chapter-2'], false, true, false))
})
