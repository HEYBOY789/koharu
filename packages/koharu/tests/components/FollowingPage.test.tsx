import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'

import { FollowingPage } from '@/components/app/FollowingPage'
import { commands, type FollowedManga } from '@koharu/bridge/protocol'

vi.mock('@/components/app/SourceImportDialog', () => ({ SourceImportDialog: () => null }))
const openUrl = vi.hoisted(() => vi.fn(async () => undefined))
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl }))
afterEach(cleanup)
const now = Math.floor(Date.now() / 1000)
const manga: FollowedManga = {
  key: 'mangadex:book:en',
  source: { kind: 'manga_dex', id: 'book', chapter_numbers_reset: false },
  title: 'Tracked manga',
  language: 'en',
  cover_url: null,
  latest: '10',
  chapters: 10,
  new_chapters: 0,
  checked_at: now,
  error: null,
}
const website: FollowedManga = {
  ...manga,
  key: 'website:book:en',
  source: { kind: 'website', url: 'https://reader.example/book' },
  title: 'Tracked website',
}
beforeEach(() => {
  vi.spyOn(commands, 'getFollowing').mockResolvedValue([manga, website])
})

it('uses native image bytes for followed MangaDex covers instead of a direct CDN image request', async () => {
  const coverUrl = 'https://uploads.mangadex.org/covers/book/cover.jpg.256.jpg'
  vi.mocked(commands.getFollowing).mockResolvedValue([{ ...manga, cover_url: coverUrl }])
  const download = vi.spyOn(commands, 'getMangadexCover').mockResolvedValue([255, 216, 255, 217])
  render(<FollowingPage active />)
  expect(await screen.findByRole('img', { name: 'Cover of Tracked manga' })).toHaveAttribute(
    'src',
    'blob:koharu-thumbnail',
  )
  expect(download).toHaveBeenCalledWith(coverUrl)
})

it('checks both source kinds, preserves badges until acknowledged and removes only the chosen follow', async () => {
  const check = vi
    .spyOn(commands, 'checkFollow')
    .mockResolvedValue([{ ...manga, latest: '11', chapters: 11, new_chapters: 1 }, website])
  const seen = vi.spyOn(commands, 'markFollowSeen').mockResolvedValue([manga, website])
  const remove = vi.spyOn(commands, 'unfollowManga').mockResolvedValue([website])
  render(<FollowingPage active />)
  await screen.findByRole('heading', { name: 'Tracked manga' })
  fireEvent.click(screen.getByRole('button', { name: 'Check all updates' }))
  await waitFor(() => expect(check).toHaveBeenCalledWith(website.key, true))
  expect(check.mock.calls).toEqual([
    [manga.key, true],
    [website.key, true],
  ])
  expect(screen.getByText('1 new chapter')).toBeVisible()
  const article = screen.getByRole('heading', { name: 'Tracked manga' }).closest('article')!
  fireEvent.click(within(article).getByRole('button', { name: 'Mark seen' }))
  await waitFor(() => expect(seen).toHaveBeenCalledWith(manga.key))
  await waitFor(() => expect(screen.queryByText('1 new chapter')).not.toBeInTheDocument())
  fireEvent.click(within(article).getByRole('button', { name: 'Unfollow' }))
  await waitFor(() => expect(remove).toHaveBeenCalledWith(manga.key))
  expect(await screen.findByRole('heading', { name: 'Tracked website' })).toBeVisible()
})

it('automatically checks stale MangaDex entries but never launches website agents on tab entry', async () => {
  vi.mocked(commands.getFollowing).mockResolvedValue([
    { ...manga, checked_at: 1 },
    { ...website, checked_at: 1 },
  ])
  const check = vi.spyOn(commands, 'checkFollow').mockResolvedValue([manga, website])
  render(<FollowingPage active />)
  await waitFor(() => expect(check).toHaveBeenCalledTimes(1))
  expect(check).toHaveBeenCalledWith(manga.key, false)
})

it('adds a website link with its chapter language and keeps the list when validation fails', async () => {
  const follow = vi.spyOn(commands, 'followManga').mockResolvedValue([manga, website])
  render(<FollowingPage active />)
  await screen.findByRole('heading', { name: 'Tracked manga' })
  fireEvent.change(screen.getByRole('textbox', { name: 'Manga source URL' }), {
    target: { value: 'bad-url' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Follow link' }))
  expect(screen.getByRole('alert')).toBeVisible()
  expect(follow).not.toHaveBeenCalled()
  fireEvent.change(screen.getByRole('textbox', { name: 'Manga source URL' }), {
    target: { value: 'https://reader.example/book' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Follow link' }))
  await waitFor(() =>
    expect(follow).toHaveBeenCalledWith({
      source: { kind: 'website', url: 'https://reader.example/book' },
      title: 'https://reader.example/book',
      language: 'en',
      cover_url: null,
    }),
  )
  expect(screen.getByRole('heading', { name: 'Tracked manga' })).toBeVisible()
})
