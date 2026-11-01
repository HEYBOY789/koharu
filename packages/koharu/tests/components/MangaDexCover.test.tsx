import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'

import { MangaDexResultCard } from '@/components/app/MangaDexResultCard'
import { followingKey, queryClient } from '@/lib/queries'
import { commands, type MangaDexEntry } from '@koharu/bridge/protocol'

const manga: MangaDexEntry = {
  id: '00000000-0000-0000-0000-000000000001',
  title: 'Atarashii Kimi e',
  url: 'https://mangadex.org/title/00000000-0000-0000-0000-000000000001',
  author: 'Author',
  artist: 'Artist',
  original_language: 'ja',
  year: 2021,
  created_at: null,
  latest_uploaded_chapter: null,
  publication_status: 'completed',
  content_rating: 'suggestive',
  description: 'Description',
  tags: [],
  cover_url: 'https://uploads.mangadex.org/covers/book/cover.jpg.256.jpg',
  chapter_numbers_reset: false,
  comparison: null,
}

function card(entry = manga, active = true, generation = 1) {
  return (
    <MangaDexResultCard
      manga={entry}
      reference='en'
      target='vi'
      cache='online'
      generation={generation}
      active={active}
      languageName={(code) => code}
      metadataLoading={false}
      sort=''
      onCreate={() => {}}
    />
  )
}
const cover = () => screen.getByRole('img', { name: 'Cover of Atarashii Kimi e' })
beforeEach(() => {
  vi.useFakeTimers()
  queryClient.setQueryData(followingKey, [])
  let id = 0
  vi.spyOn(URL, 'createObjectURL').mockImplementation(() => `blob:cover-${++id}`)
  vi.spyOn(commands, 'getMangadexCover').mockResolvedValue([255, 216, 255, 217])
})
afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

it('loads a local blob after a transient native failure without another chapter lookup or replacing the result card', async () => {
  const chapters = vi.spyOn(commands, 'getMangadexChapters')
  vi.mocked(commands.getMangadexCover).mockRejectedValueOnce(new Error('Temporary CDN failure'))
  render(card())
  await act(async () => {
    await Promise.resolve()
  })
  await act(async () => vi.advanceTimersByTimeAsync(10000))
  const retried = cover()
  expect(retried.getAttribute('src')).toMatch(/^blob:/)
  expect(commands.getMangadexCover).toHaveBeenCalledTimes(2)
  fireEvent.load(retried)
  await act(async () => vi.advanceTimersByTimeAsync(60000))
  expect(cover()).toBe(retried)
  expect(screen.getByRole('heading', { name: manga.title })).toBeVisible()
  expect(chapters).not.toHaveBeenCalled()
})

it('stops automatic attempts and lets the user reload after the network recovers', async () => {
  vi.mocked(commands.getMangadexCover)
    .mockRejectedValueOnce(new Error('CDN unavailable'))
    .mockRejectedValueOnce(new Error('CDN unavailable'))
    .mockRejectedValueOnce(new Error('CDN unavailable'))
  render(card())
  await act(async () => {
    await Promise.resolve()
  })
  // Flush each React effect before advancing the next retry timer.
  for (let attempt = 0; attempt < 2; attempt++) {
    await act(async () => vi.advanceTimersByTimeAsync(10000))
  }
  await act(async () => vi.advanceTimersByTimeAsync(60000))
  expect(commands.getMangadexCover).toHaveBeenCalledTimes(3)
  expect(screen.queryByRole('img', { name: 'Cover of Atarashii Kimi e' })).not.toBeInTheDocument()
  fireEvent.click(screen.getByRole('button', { name: 'Reload cover for Atarashii Kimi e' }))
  await act(async () => {
    await Promise.resolve()
  })
  fireEvent.load(cover())
  await act(async () => vi.advanceTimersByTimeAsync(60000))
  expect(commands.getMangadexCover).toHaveBeenCalledTimes(4)
  expect(cover()).toBeVisible()
})

it('pauses retries off-tab and resumes without restarting chapter work', async () => {
  vi.mocked(commands.getMangadexCover).mockRejectedValueOnce(new Error('Temporary CDN failure'))
  const view = render(card())
  await act(async () => {
    await Promise.resolve()
  })
  view.rerender(card(manga, false))
  await act(async () => vi.advanceTimersByTimeAsync(60000))
  expect(commands.getMangadexCover).toHaveBeenCalledTimes(1)
  view.rerender(card(manga, true))
  await act(async () => vi.advanceTimersByTimeAsync(10000))
  expect(cover()).toBeVisible()
  expect(commands.getMangadexCover).toHaveBeenCalledTimes(2)
})

it('discards stale download results, resets for a changed URL or search and releases old blob URLs', async () => {
  let oldResult!: (bytes: number[]) => void
  vi.mocked(commands.getMangadexCover).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        oldResult = resolve
      }),
  )
  const view = render(card())
  const replacement = {
    ...manga,
    cover_url: 'https://uploads.mangadex.org/covers/book/new.jpg.256.jpg',
  }
  view.rerender(card(replacement))
  await act(async () => {
    await Promise.resolve()
  })
  const latest = cover()
  const latestUrl = latest.getAttribute('src')
  await act(async () => oldResult([0]))
  expect(cover()).toBe(latest)
  expect(URL.createObjectURL).toHaveBeenCalledTimes(1)
  fireEvent.load(latest)
  view.rerender(card(replacement, true, 2))
  await act(async () => {
    await Promise.resolve()
  })
  expect(cover()).not.toBe(latest)
  expect(URL.revokeObjectURL).toHaveBeenCalledWith(latestUrl)
  const finalUrl = cover().getAttribute('src')
  view.unmount()
  expect(URL.revokeObjectURL).toHaveBeenCalledWith(finalUrl)
})
