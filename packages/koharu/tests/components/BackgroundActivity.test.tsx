import { QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

import { ActivityCenter } from '@/components/editor/ActivityCenter'
import { pagesKey, projectKey, queryClient, useProjectBusy } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands, type Job } from '@koharu/bridge/protocol'

afterEach(cleanup)

const job: Job = {
  id: 'background-job',
  project: 'B',
  state: 'running',
  completed: 8,
  total: 20,
  page: 'b-page',
  chapter: 'b-chapter',
  chapter_title: 'Chapter 8',
  phase: 'translate',
  stage: 'translation',
  model: 'openai-compatible',
  detail: null,
  error: null,
  retry: null,
}

function EditingState() {
  const busy = useProjectBusy()
  return <button disabled={busy}>Edit current project</button>
}

function setup() {
  queryClient.setQueryData(projectKey, {
    name: 'A',
    revision: 1,
    active_page: 'a-page',
    can_undo: true,
    can_redo: false,
  })
  useKoharuStore.setState({
    jobs: { [job.id]: job },
    selectedLayers: ['a-layer'],
    settingsOpen: true,
    workspaceTab: 'mangadex',
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <EditingState />
      <ActivityCenter />
    </QueryClientProvider>,
  )
}

it('keeps editing available while the background job stays stoppable and can be opened', async () => {
  const stop = vi.spyOn(commands, 'stopJob').mockResolvedValue(null)
  const open = vi.spyOn(commands, 'openProject').mockResolvedValue(null)
  const pages = vi.spyOn(commands, 'getPages').mockResolvedValue([])
  setup()
  queryClient.setQueryData(pagesKey, [{ id: 'b-page', label: 'A cached page' }])
  expect(screen.getByText('Edit current project')).toBeEnabled()
  expect(screen.getByRole('button', { name: 'Activity details' })).toHaveAttribute(
    'aria-expanded',
    'false',
  )
  expect(screen.queryByRole('region', { name: 'Activity details' })).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Stop' }))
  await waitFor(() => expect(stop).toHaveBeenCalledWith('background-job'))
  fireEvent.click(screen.getByRole('button', { name: 'Open project “B”' }))
  await waitFor(() => expect(open).toHaveBeenCalledWith('B'))
  expect(useKoharuStore.getState().settingsOpen).toBe(false)
  expect(useKoharuStore.getState().workspaceTab).toBe('projects')
  fireEvent.click(screen.getByRole('button', { name: 'Activity details' }))
  const details = screen.getByRole('region', { name: 'Activity details' })
  expect(within(details).getByText('B')).toBeVisible()
  expect(within(details).getByText('Translating “Chapter 8”')).toBeVisible()
  expect(within(details).queryByText(/A cached page/)).not.toBeInTheDocument()
  expect(pages).not.toHaveBeenCalled()
  expect(useKoharuStore.getState().selectedLayers).toEqual(['a-layer'])
  act(() => useKoharuStore.setState({ jobs: {} }))
  expect(screen.queryByRole('button', { name: 'Activity details' })).not.toBeInTheDocument()
  act(() => useKoharuStore.setState({ jobs: { [job.id]: job } }))
  expect(screen.getByRole('button', { name: 'Activity details' })).toHaveAttribute(
    'aria-expanded',
    'false',
  )
})

it('keeps long failures collapsed until requested and keeps completed project navigation', async () => {
  setup()
  const error = ('HTTP 524: ' + 'origin-response-timeout '.repeat(120)).trim()
  act(() => useKoharuStore.setState({ jobs: { [job.id]: { ...job, state: 'failed', error } } }))
  expect(screen.queryByText(error)).not.toBeInTheDocument()
  expect(screen.getByLabelText('Needs attention: 1')).toBeVisible()
  expect(screen.getByRole('button', { name: 'Activity details' })).toHaveAttribute(
    'aria-expanded',
    'false',
  )
  fireEvent.click(screen.getByRole('button', { name: 'Activity details' }))
  expect(screen.getByText(error)).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Activity details' }))
  act(() =>
    useKoharuStore.setState({ jobs: { [job.id]: { ...job, state: 'finished', completed: 20 } } }),
  )
  expect(screen.getByRole('button', { name: 'Activity details' })).toHaveAttribute(
    'aria-expanded',
    'false',
  )
  expect(screen.getByRole('button', { name: 'Open project “B”' })).toBeEnabled()
  expect(screen.queryByLabelText('Needs attention: 1')).not.toBeInTheDocument()
  expect(useKoharuStore.getState().selectedLayers).toEqual(['a-layer'])
})

it('stops a queued import through the same job action and leaves editing available', async () => {
  setup()
  const queued: Job = {
    ...job,
    id: 'queued-import',
    project: 'New manga',
    state: 'queued',
    phase: 'import',
    page: null,
    chapter: null,
  }
  act(() => useKoharuStore.setState({ jobs: { [queued.id]: queued } }))
  const stop = vi.spyOn(commands, 'stopJob').mockResolvedValue(null)
  expect(screen.getByRole('button', { name: 'Edit current project' })).toBeEnabled()
  expect(screen.queryByRole('button', { name: 'Open project “New manga”' })).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Stop' }))
  await waitFor(() => expect(stop).toHaveBeenCalledWith(queued.id))
  expect(useKoharuStore.getState().selectedLayers).toEqual(['a-layer'])
})
