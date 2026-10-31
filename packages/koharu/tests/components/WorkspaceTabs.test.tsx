import { QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'

import { KoharuApp } from '@/components/app/KoharuApp'
import { projectKey, queryClient } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands } from '@koharu/bridge/protocol'

vi.mock('@/components/editor/Editor', () => ({
  Editor: () => {
    const [text, setText] = useState('draft')
    return (
      <label>
        Editor draft
        <input value={text} onChange={(event) => setText(event.target.value)} />
      </label>
    )
  },
}))
vi.mock('@/components/preferences/SettingsPage', () => ({ SettingsPage: () => <p>Settings</p> }))

afterEach(cleanup)
beforeEach(() => {
  vi.spyOn(commands, 'getProject').mockResolvedValue(null)
  vi.spyOn(commands, 'listProjects').mockResolvedValue([])
  vi.spyOn(commands, 'getPages').mockResolvedValue([])
  vi.spyOn(commands, 'getPage').mockResolvedValue(null)
  vi.spyOn(commands, 'getChapters').mockResolvedValue([])
  vi.spyOn(commands, 'getMangadexOptions').mockResolvedValue({
    options: [],
    checked_at: 1700000000,
  })
})
const renderApp = () =>
  render(
    <QueryClientProvider client={queryClient}>
      <KoharuApp />
    </QueryClientProvider>,
  )

it('opens MangaDex beside Projects without opening a project and retains the search across tab changes', async () => {
  const user = userEvent.setup()
  const open = vi.spyOn(commands, 'openProject')
  const create = vi.spyOn(commands, 'createProject')
  const search = vi.spyOn(commands, 'searchMangadex').mockResolvedValue({
    manga: [],
    total: 0,
    scanned: 0,
    skipped: 0,
    next_offset: null,
    capped: false,
    checked_at: 1700000000,
    network_requests: 1,
  })
  renderApp()
  await screen.findByRole('heading', { name: 'Projects' })
  await user.click(screen.getByRole('tab', { name: 'MangaDex' }))
  expect(screen.getByRole('tab', { name: 'MangaDex' })).toHaveAttribute('aria-selected', 'true')
  await user.type(screen.getByRole('textbox', { name: 'Manga title (optional)' }), 'animan')
  await user.click(screen.getByRole('button', { name: 'Search' }))
  await waitFor(() =>
    expect(search).toHaveBeenCalledWith(expect.objectContaining({ title: 'animan' })),
  )
  await user.click(screen.getByRole('tab', { name: 'Projects' }))
  expect(screen.getByRole('heading', { name: 'Projects' })).toBeVisible()
  await user.click(screen.getByRole('tab', { name: 'MangaDex' }))
  expect(screen.getByRole('textbox', { name: 'Manga title (optional)' })).toHaveValue('animan')
  expect(search).toHaveBeenCalledTimes(1)
  expect(open).not.toHaveBeenCalled()
  expect(create).not.toHaveBeenCalled()
})

it('keeps the open editor state, selected layer and running job while visiting MangaDex', async () => {
  const project = {
    name: 'Book A',
    revision: 1,
    active_page: null,
    can_undo: false,
    can_redo: false,
  }
  queryClient.setQueryData(projectKey, project)
  vi.mocked(commands.getProject).mockResolvedValue(project)
  const job = {
    id: 'b',
    project: 'Book B',
    state: 'running' as const,
    completed: 3,
    total: 10,
    page: null,
    chapter: null,
    chapter_title: null,
    phase: null,
    stage: null,
    model: null,
    detail: null,
    retry: null,
    error: null,
  }
  useKoharuStore.setState({
    selectedPages: ['page-a'],
    selectedLayers: ['layer-a'],
    jobs: { b: job },
  })
  const close = vi.spyOn(commands, 'closeProject')
  const stop = vi.spyOn(commands, 'stopJob')
  const user = userEvent.setup()
  renderApp()
  fireEvent.change(await screen.findByRole('textbox', { name: 'Editor draft' }), {
    target: { value: 'unsaved text' },
  })
  await user.click(screen.getByRole('tab', { name: 'MangaDex' }))
  expect(screen.getByRole('heading', { name: 'Find manga on MangaDex' })).toBeVisible()
  await user.click(screen.getByRole('tab', { name: 'Projects' }))
  expect(screen.getByRole('textbox', { name: 'Editor draft' })).toHaveValue('unsaved text')
  await user.click(screen.getByRole('button', { name: 'Project library' }))
  expect(await screen.findByRole('heading', { name: 'Projects' })).toBeVisible()
  await user.click(screen.getByRole('button', { name: 'Return to Book A' }))
  expect(screen.getByRole('textbox', { name: 'Editor draft' })).toHaveValue('unsaved text')
  expect(useKoharuStore.getState().selectedLayers).toEqual(['layer-a'])
  expect(useKoharuStore.getState().jobs.b).toEqual(job)
  expect(close).not.toHaveBeenCalled()
  expect(stop).not.toHaveBeenCalled()
})
