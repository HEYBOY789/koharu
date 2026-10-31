import { QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

import { ActivityCenter } from '@/components/editor/ActivityCenter'
import { StopJobButton } from '@/components/editor/StopJobButton'
import { queryClient } from '@/lib/queries'
import { receiveJob, useKoharuStore } from '@/lib/store'
import { commands, type Job } from '@koharu/bridge/protocol'

afterEach(cleanup)

it('keeps a recovering job visible and lets Stop cancel the wait through the existing command', async () => {
  const job: Job = {
    id: 'recovering',
    project: 'Book',
    state: 'running',
    completed: 12,
    total: 100,
    page: null,
    chapter: 'chapter-6',
    chapter_title: 'Chapter 6',
    phase: 'translate',
    stage: 'translation',
    model: 'openai-compatible',
    detail: null,
    error: null,
    retry: { attempt: 2, max_attempts: 5, delay_seconds: 30 },
  }
  useKoharuStore.setState({ jobs: { [job.id]: job } })
  const stop = vi.spyOn(commands, 'stopJob').mockResolvedValue(null)
  const view = render(
    <QueryClientProvider client={queryClient}>
      <ActivityCenter />
    </QueryClientProvider>,
  )
  expect(screen.queryByRole('status')).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Activity details' }))
  expect(screen.getByRole('status')).toHaveTextContent('30')
  expect(screen.getByRole('status')).toHaveTextContent('2/5')
  fireEvent.click(screen.getAllByRole('button', { name: 'Stop' })[0])
  await waitFor(() => expect(stop).toHaveBeenCalledWith('recovering'))
  expect(
    screen
      .getAllByRole('button', { name: /Stopping/ })
      .every((button) => (button as HTMLButtonElement).disabled),
  ).toBe(true)
  expect(screen.getByRole('status')).toHaveTextContent(/current operation/i)
  fireEvent.click(screen.getAllByRole('button', { name: /Stopping/ })[0])
  expect(stop).toHaveBeenCalledTimes(1)
  act(() => useKoharuStore.setState({ jobs: { [job.id]: { ...job, retry: null } } }))
  view.rerender(
    <QueryClientProvider client={queryClient}>
      <ActivityCenter />
    </QueryClientProvider>,
  )
  expect(screen.getByRole('status')).toHaveTextContent(/current operation/i)
  act(() => receiveJob({ ...job, state: 'stopped', retry: null }))
  expect(screen.queryByRole('button', { name: /Stopping/ })).not.toBeInTheDocument()
  expect(useKoharuStore.getState().stoppingJobs[job.id]).toBeUndefined()
})

it('allows another Stop request when the native command fails', async () => {
  const request = Promise.withResolvers<null>()
  const stop = vi
    .spyOn(commands, 'stopJob')
    .mockReturnValueOnce(request.promise)
    .mockResolvedValue(null)
  render(<StopJobButton id='job' />)
  fireEvent.click(screen.getByRole('button', { name: /^Stop$/ }))
  expect(screen.getByRole('button', { name: /Stopping/ })).toBeDisabled()
  await act(async () => request.reject(new Error('temporary native failure')))
  fireEvent.click(screen.getByRole('button', { name: /^Stop$/ }))
  await waitFor(() => expect(stop).toHaveBeenCalledTimes(2))
})
