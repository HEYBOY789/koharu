import { QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

import { ActivityCenter } from '@/components/editor/ActivityCenter'
import { queryClient } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
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
  act(() => useKoharuStore.setState({ jobs: { [job.id]: { ...job, retry: null } } }))
  view.rerender(
    <QueryClientProvider client={queryClient}>
      <ActivityCenter />
    </QueryClientProvider>,
  )
  expect(screen.queryByRole('status')).not.toBeInTheDocument()
  expect(screen.getByText('Translating “Chapter 6”')).toBeVisible()
})
