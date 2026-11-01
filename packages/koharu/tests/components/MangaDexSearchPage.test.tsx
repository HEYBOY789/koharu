import { act, fireEvent, render, screen, waitFor, cleanup, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'

import { MangaDexSearchPage } from '@/components/app/MangaDexSearchPage'
import { mangaDexDefaults } from '@/lib/mangadex'
import { useKoharuStore } from '@/lib/store'
import { commands, type MangaDexPage } from '@koharu/bridge/protocol'

const openUrl = vi.hoisted(() => vi.fn(async () => undefined))
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl }))
afterEach(cleanup)
const tag = { id: '00000000-0000-0000-0000-000000000002', name: 'Action', group: 'genre' }
beforeEach(() => {
  vi.spyOn(commands, 'getMangadexCover').mockResolvedValue([255, 216, 255, 217])
  vi.spyOn(commands, 'getFollowing').mockResolvedValue([])
  vi.spyOn(commands, 'getMangadexMetadata').mockImplementation(async (input) => ({
    manga: input.map(({ id }) => ({
      id,
      rating: 8.125,
      follows: 123456,
      latest_upload_at: '2026-10-27T03:05:27+00:00',
    })),
    checked_at: 1700000000,
    network_requests: 2,
  }))
  vi.spyOn(commands, 'getMangadexOptions').mockResolvedValue({
    options: [tag],
    checked_at: 1700000000,
  })
})
const empty: MangaDexPage = {
  manga: [],
  scanned: 10,
  skipped: 10,
  next_offset: 10,
  capped: false,
  total: 100,
  checked_at: 1700000000,
  network_requests: 1,
}
const match: MangaDexPage = {
  ...empty,
  next_offset: null,
  manga: [
    {
      id: '00000000-0000-0000-0000-000000000001',
      title: 'A Korean series',
      url: 'https://mangadex.org/title/00000000-0000-0000-0000-000000000001',
      author: 'Author',
      artist: 'Artist',
      original_language: 'ko',
      year: 2023,
      created_at: '2018-02-06T10:59:58+00:00',
      latest_uploaded_chapter: '00000000-0000-0000-0000-000000000004',
      publication_status: 'ongoing',
      content_rating: 'safe',
      tags: ['Action'],
      description: 'A summary',
      cover_url: 'https://uploads.mangadex.org/covers/book/cover.jpg.256.jpg',
      chapter_numbers_reset: false,
      comparison: {
        reference_language: 'ko',
        target_language: 'vi',
        total_available: null,
        latest_available: null,
        missing: 5,
        status: 'behind',
        checked_at: Math.floor(Date.now() / 1000),
        network_requests: 0,
        chapters: [
          { language: 'ko', latest: '20', count: 20, unnumbered: 0 },
          { language: 'vi', latest: '15', count: 15, unnumbered: 0 },
        ],
      },
    },
  ],
}

it('follows the selected chapter language from a result without creating a project', async () => {
  vi.spyOn(commands, 'searchMangadex').mockResolvedValue(match)
  const follow = vi.spyOn(commands, 'followManga').mockResolvedValue([])
  const create = vi.spyOn(commands, 'importSourceUrl')
  render(<MangaDexSearchPage active />)
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  fireEvent.click(await screen.findByRole('button', { name: 'Follow English' }))
  await waitFor(() =>
    expect(follow).toHaveBeenCalledWith({
      source: { kind: 'manga_dex', id: match.manga[0].id, chapter_numbers_reset: false },
      title: match.manga[0].title,
      language: 'en',
      cover_url: match.manga[0].cover_url,
    }),
  )
  expect(create).not.toHaveBeenCalled()
})

it('lists all titles on demand and encodes tag logic, sort, year and multiple original languages', async () => {
  const search = vi.spyOn(commands, 'searchMangadex').mockResolvedValue(empty)
  const user = userEvent.setup()
  render(<MangaDexSearchPage active />)
  expect(search).not.toHaveBeenCalled()
  await user.click(screen.getByRole('button', { name: 'Search' }))
  await waitFor(() => expect(search).toHaveBeenCalledWith(mangaDexDefaults()))
  await user.click(screen.getByText('Advanced filters'))
  await screen.findByRole('button', { name: 'Action: Ignore' })
  fireEvent.change(screen.getByRole('combobox', { name: 'Sort by' }), {
    target: { value: 'rating.desc' },
  })
  fireEvent.change(screen.getByRole('spinbutton', { name: 'Publication year' }), {
    target: { value: '2023' },
  })
  await user.click(screen.getByRole('button', { name: 'Action: Ignore' }))
  await user.click(screen.getByRole('checkbox', { name: 'Include all (AND)' }))
  await user.click(screen.getByRole('button', { name: 'Original language' }))
  await user.click(await screen.findByRole('checkbox', { name: 'Japanese' }))
  await user.click(screen.getByRole('checkbox', { name: 'Korean' }))
  await user.keyboard('{Escape}')
  await user.click(screen.getByRole('button', { name: 'Search' }))
  await waitFor(() =>
    expect(search).toHaveBeenLastCalledWith(
      expect.objectContaining({
        sort: 'rating.desc',
        year: 2023,
        original_languages: ['ja', 'ko'],
        included_tags: [tag.id],
        excluded_tags: [],
        include_all: true,
      }),
    ),
  )
  await user.click(screen.getByRole('button', { name: 'Action: Include' }))
  await user.click(screen.getByRole('button', { name: 'Search' }))
  await waitFor(() =>
    expect(search).toHaveBeenLastCalledWith(
      expect.objectContaining({ included_tags: [], excluded_tags: [tag.id] }),
    ),
  )
  await user.click(screen.getByRole('button', { name: 'Reset filters' }))
  await user.click(screen.getByRole('button', { name: 'Search' }))
  await waitFor(() => expect(search).toHaveBeenLastCalledWith(mangaDexDefaults()))
})

it('continues an empty comparison batch, preserves reference language, opens a result and refreshes from page one', async () => {
  const search = vi
    .spyOn(commands, 'searchMangadex')
    .mockResolvedValueOnce(empty)
    .mockResolvedValue(match)
  render(<MangaDexSearchPage active />)
  fireEvent.change(screen.getByRole('combobox', { name: 'Translation progress' }), {
    target: { value: 'either' },
  })
  fireEvent.change(screen.getByRole('combobox', { name: 'Reference language' }), {
    target: { value: 'ko' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  expect(await screen.findByText(/No matches in the checked titles/)).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Next results' }))
  expect(await screen.findByRole('heading', { name: 'A Korean series' })).toBeVisible()
  expect(search).toHaveBeenNthCalledWith(
    2,
    expect.objectContaining({ offset: 10, cache: 'online', reference: 'ko' }),
  )
  expect(screen.getAllByText('Ch. 20')).toHaveLength(2)
  fireEvent.click(screen.getByRole('button', { name: 'View on MangaDex' }))
  expect(openUrl).toHaveBeenCalledWith(match.manga[0].url)
  fireEvent.click(screen.getByRole('button', { name: 'Refresh from API' }))
  await waitFor(() =>
    expect(search).toHaveBeenLastCalledWith(
      expect.objectContaining({ offset: 0, cache: 'refresh' }),
    ),
  )
  fireEvent.change(screen.getByRole('combobox', { name: 'Translation progress' }), {
    target: { value: 'no_target' },
  })
  expect(screen.getByRole('heading', { name: 'A Korean series' })).toBeVisible()
  expect(screen.getByText(/Filters have changed/)).toBeVisible()
})

it('keeps a background translation and selection intact on offline errors and does not refresh offline', async () => {
  const job = {
    id: 'translating',
    project: 'B',
    state: 'running' as const,
    completed: 3,
    total: 10,
    page: null,
    chapter: null,
    chapter_title: null,
    chapter_progress: null,
    phase: null,
    stage: null,
    model: null,
    detail: null,
    retry: null,
    error: null,
  }
  useKoharuStore.setState({ jobs: { [job.id]: job }, selectedLayers: ['a-layer'] })
  const search = vi
    .spyOn(commands, 'searchMangadex')
    .mockRejectedValue(new Error('No saved MangaDex response'))
  const stop = vi.spyOn(commands, 'stopJob')
  const process = vi.spyOn(commands, 'process')
  const user = userEvent.setup()
  render(<MangaDexSearchPage active />)
  await user.click(screen.getByRole('checkbox', { name: 'Use saved cache offline' }))
  expect(screen.getByRole('button', { name: 'Refresh from API' })).toBeDisabled()
  await user.click(screen.getByRole('button', { name: 'Search' }))
  expect(await screen.findByText('Error: No saved MangaDex response')).toBeVisible()
  expect(search).toHaveBeenCalledWith(expect.objectContaining({ cache: 'offline' }))
  expect(useKoharuStore.getState().jobs.translating).toEqual(job)
  expect(useKoharuStore.getState().selectedLayers).toEqual(['a-layer'])
  expect(stop).not.toHaveBeenCalled()
  expect(process).not.toHaveBeenCalled()
})

it('debounces people lookup and uses selected author IDs without searching the manga list on each keystroke', async () => {
  const person = { id: '00000000-0000-0000-0000-000000000003', name: 'Takuya Okada', group: '' }
  const lookup = vi.spyOn(commands, 'getMangadexOptions').mockImplementation(async (kind) => ({
    options: kind === 'tags' ? [tag] : [person],
    checked_at: 1700000000,
  }))
  const search = vi.spyOn(commands, 'searchMangadex').mockResolvedValue(empty)
  const user = userEvent.setup()
  render(<MangaDexSearchPage active />)
  await user.click(screen.getByText('Advanced filters'))
  await user.type(screen.getByRole('textbox', { name: 'Authors' }), 'Takuya')
  await user.click(await screen.findByRole('button', { name: person.name }))
  expect(lookup.mock.calls.filter(([kind]) => kind === 'people')).toHaveLength(1)
  expect(search).not.toHaveBeenCalled()
  await user.click(screen.getByRole('button', { name: 'Search' }))
  await waitFor(() =>
    expect(search).toHaveBeenCalledWith(expect.objectContaining({ authors: [person.id] })),
  )
})

it('bounds the mounted result cards and pages through a fetched batch without more API requests', async () => {
  const books = Array.from({ length: 100 }, (_, index) => ({
    ...match.manga[0],
    id: `book-${index}`,
    title: `Book ${index}`,
  }))
  const search = vi
    .spyOn(commands, 'searchMangadex')
    .mockResolvedValue({ ...match, manga: books, scanned: 100, total: 100, next_offset: null })
  const user = userEvent.setup()
  render(<MangaDexSearchPage active />)
  await user.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByRole('heading', { name: 'Book 0' })
  expect(screen.getAllByRole('article')).toHaveLength(20)
  expect(screen.queryByRole('heading', { name: 'Book 20' })).not.toBeInTheDocument()
  await user.click(screen.getByRole('button', { name: 'Next results' }))
  expect(screen.getByRole('heading', { name: 'Book 20' })).toBeVisible()
  expect(screen.queryByRole('heading', { name: 'Book 0' })).not.toBeInTheDocument()
  expect(screen.getAllByRole('article')).toHaveLength(20)
  await user.click(screen.getByRole('button', { name: 'Previous results' }))
  expect(screen.getByRole('heading', { name: 'Book 0' })).toBeVisible()
  expect(search).toHaveBeenCalledTimes(1)
  expect(commands.getMangadexMetadata).toHaveBeenCalledTimes(1)
})

it('keeps the applied filters and API sort when jumping to an unfetched page', async () => {
  const first = match.manga[0]
  const second = { ...first, id: '00000000-0000-0000-0000-000000000002', title: 'A second title' }
  const third = { ...first, id: '00000000-0000-0000-0000-000000000003', title: 'A third title' }
  const search = vi
    .spyOn(commands, 'searchMangadex')
    .mockResolvedValueOnce({
      ...match,
      manga: [
        first,
        second,
        ...Array.from({ length: 98 }, (_, i) => ({
          ...first,
          id: `book-${i}`,
          title: `Book ${i}`,
        })),
      ],
      scanned: 100,
      total: 101,
      next_offset: 100,
    })
    .mockResolvedValue({ ...match, manga: [third], scanned: 1, total: 101 })
  vi.spyOn(commands, 'getMangadexMetadata').mockImplementation(async (input) => ({
    manga: [...input].reverse().map(({ id }) => ({
      id,
      rating: id === first.id ? 8.125 : 9.5,
      follows: 123456,
      latest_upload_at: '2026-10-27T03:05:27+00:00',
    })),
    checked_at: 1700000000,
    network_requests: 2,
  }))
  render(<MangaDexSearchPage active />)
  fireEvent.change(screen.getByRole('combobox', { name: 'Sort by' }), {
    target: { value: 'rating.desc' },
  })
  expect(search).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByText('8.13 / 10')
  await screen.findAllByText('9.50 / 10')
  expect(screen.getAllByText('123,456')).toHaveLength(20)
  expect(commands.getMangadexMetadata).toHaveBeenCalledTimes(1)
  expect(commands.getMangadexMetadata).toHaveBeenCalledWith(
    expect.arrayContaining([
      { id: first.id, latest_uploaded_chapter: first.latest_uploaded_chapter },
      { id: second.id, latest_uploaded_chapter: second.latest_uploaded_chapter },
    ]),
    'online',
  )
  expect(
    screen
      .getAllByRole('heading', { level: 3 })
      .slice(0, 2)
      .map((item) => item.textContent),
  ).toEqual([first.title, second.title])
  fireEvent.change(screen.getByRole('combobox', { name: 'Sort by' }), {
    target: { value: 'title.asc' },
  })
  fireEvent.change(screen.getByRole('combobox', { name: 'Reference language' }), {
    target: { value: 'ja' },
  })
  fireEvent.change(screen.getByRole('combobox', { name: 'Translation language' }), {
    target: { value: 'fr' },
  })
  expect(search).toHaveBeenCalledTimes(1)
  expect(screen.getByRole('heading', { name: first.title })).toBeVisible()
  expect(screen.getByText(/Filters have changed/)).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Page 6' }))
  await screen.findByRole('heading', { name: third.title })
  expect(search).toHaveBeenNthCalledWith(
    2,
    expect.objectContaining({ sort: 'rating.desc', offset: 100, reference: 'en', target: 'vi' }),
  )
  expect(screen.getAllByRole('heading', { level: 3 }).map((item) => item.textContent)).toEqual([
    third.title,
  ])
})

it('jumps directly to the last partial page, first page and a typed cached page without scanning between them', async () => {
  const books = (offset: number, count: number) =>
    Array.from({ length: count }, (_, i) => ({
      ...match.manga[0],
      id: `book-${offset + i}`,
      title: `Book ${offset + i}`,
    }))
  const search = vi.spyOn(commands, 'searchMangadex').mockImplementation(async ({ offset }) => ({
    ...match,
    total: 207,
    scanned: offset === 200 ? 7 : 100,
    manga: books(offset, offset === 200 ? 7 : 100),
    next_offset: offset === 200 ? null : offset + 100,
  }))
  render(<MangaDexSearchPage active />)
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByRole('heading', { name: 'Book 0' })
  expect(screen.getByText('Page 1 of 11')).toBeVisible()
  const results = screen.getByRole('region', { name: 'Results' })
  const viewport = results.parentElement!
  viewport.scrollTop = 500
  vi.spyOn(viewport, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 50, 1000, 500))
  vi.spyOn(results, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 150, 1000, 1500))
  vi.mocked(Element.prototype.scrollIntoView).mockClear()
  fireEvent.click(screen.getByRole('button', { name: 'Page 11' }))
  await screen.findByRole('heading', { name: 'Book 200' })
  expect(viewport.scrollTop).toBe(600)
  expect(Element.prototype.scrollIntoView).not.toHaveBeenCalled()
  expect(screen.getAllByRole('article')).toHaveLength(7)
  expect(search.mock.calls.map(([input]) => input.offset)).toEqual([0, 200])
  expect(screen.getByText('Page 11 of 11')).toBeVisible()
  expect(screen.getByRole('button', { name: 'Next results' })).toBeDisabled()
  fireEvent.click(screen.getByRole('button', { name: 'Page 1' }))
  expect(screen.getByRole('heading', { name: 'Book 0' })).toBeVisible()
  fireEvent.change(screen.getByRole('spinbutton', { name: 'Go to page' }), {
    target: { value: '3' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Go' }))
  expect(screen.getByRole('heading', { name: 'Book 40' })).toBeVisible()
  expect(search).toHaveBeenCalledTimes(2)
  fireEvent.change(screen.getByRole('spinbutton', { name: 'Go to page' }), {
    target: { value: '12' },
  })
  expect(screen.getByRole('button', { name: 'Go' })).toBeDisabled()
  fireEvent.change(screen.getByRole('spinbutton', { name: 'Go to page' }), {
    target: { value: '' },
  })
  expect(screen.getByRole('button', { name: 'Go' })).toBeDisabled()
})

it('keeps boundary pages and compact ellipses around the current page', async () => {
  const search = vi.spyOn(commands, 'searchMangadex').mockImplementation(async ({ offset }) => ({
    ...match,
    total: 1000,
    scanned: 100,
    manga: Array.from({ length: 100 }, (_, i) => ({
      ...match.manga[0],
      id: `book-${offset + i}`,
      title: `Book ${offset + i}`,
    })),
    next_offset: offset === 900 ? null : offset + 100,
  }))
  render(<MangaDexSearchPage active />)
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByRole('heading', { name: 'Book 0' })
  const pagination = screen.getByRole('navigation', { name: 'Result pages' })
  const pages = () => within(pagination).getAllByRole('button', { name: /^Page \d+$/ })
  const numbers = () => pages().map((button) => button.textContent)
  const gaps = () => pagination.querySelectorAll('[data-slot="pagination-ellipsis"]')
  expect(numbers()).toEqual(['1', '2', '3', '4', '5', '50'])
  expect(gaps()).toHaveLength(1)
  fireEvent.change(screen.getByRole('spinbutton', { name: 'Go to page' }), {
    target: { value: '6' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Go' }))
  await screen.findByRole('heading', { name: 'Book 100' })
  expect(numbers()).toEqual(['1', '4', '5', '6', '7', '8', '50'])
  expect(gaps()).toHaveLength(2)
  expect(within(pagination).getByRole('button', { name: 'Page 6' })).toHaveAttribute(
    'aria-current',
    'page',
  )
  fireEvent.click(within(pagination).getByRole('button', { name: 'Page 50' }))
  await screen.findByRole('heading', { name: 'Book 980' })
  expect(numbers()).toEqual(['1', '46', '47', '48', '49', '50'])
  expect(gaps()).toHaveLength(1)
  expect(within(pagination).getByRole('button', { name: 'Next results' })).toBeDisabled()
  fireEvent.click(within(pagination).getByRole('button', { name: 'Page 1' }))
  expect(screen.getByRole('heading', { name: 'Book 0' })).toBeVisible()
  expect(within(pagination).getByRole('button', { name: 'Previous results' })).toBeDisabled()
  expect(search.mock.calls.map(([input]) => input.offset)).toEqual([0, 100, 900])
})

it('caps direct page navigation at the API limit and preserves the current results when a page fails', async () => {
  const search = vi
    .spyOn(commands, 'searchMangadex')
    .mockResolvedValueOnce({ ...match, total: 12000 })
    .mockRejectedValueOnce(new Error('No saved response for this page'))
    .mockResolvedValue({ ...match, total: 12000 })
  render(<MangaDexSearchPage active />)
  fireEvent.click(screen.getByRole('checkbox', { name: 'Use saved cache offline' }))
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByRole('heading', { name: match.manga[0].title })
  expect(screen.getByText('Page 1 of 500')).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Page 500' }))
  await screen.findByText('Error: No saved response for this page')
  expect(search).toHaveBeenLastCalledWith(
    expect.objectContaining({ offset: 9900, cache: 'offline' }),
  )
  expect(screen.getByText('Page 1 of 500')).toBeVisible()
  expect(screen.getByRole('heading', { name: match.manga[0].title })).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Page 500' }))
  await screen.findByText('Page 500 of 500')
  expect(search.mock.calls.map(([input]) => input.offset)).toEqual([0, 9900, 9900])
})

it('keeps previous results during a pending or failed replacement search and after resetting draft filters', async () => {
  let rejectSearch!: (cause: Error) => void
  const search = vi
    .spyOn(commands, 'searchMangadex')
    .mockResolvedValueOnce(match)
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          rejectSearch = reject
        }),
    )
  render(<MangaDexSearchPage active />)
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByRole('heading', { name: match.manga[0].title })
  fireEvent.change(screen.getByRole('textbox', { name: 'Manga title (optional)' }), {
    target: { value: 'New title' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  expect(screen.getByRole('heading', { name: match.manga[0].title })).toBeVisible()
  await act(async () => rejectSearch(new Error('Network unavailable')))
  await screen.findByText('Error: Network unavailable')
  expect(screen.getByRole('heading', { name: match.manga[0].title })).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Reset filters' }))
  expect(screen.getByRole('heading', { name: match.manga[0].title })).toBeVisible()
  expect(search).toHaveBeenCalledTimes(2)
})

it('jumps over unchecked comparison groups and can return from an empty last group', async () => {
  const search = vi
    .spyOn(commands, 'searchMangadex')
    .mockResolvedValueOnce({ ...match, total: 27, scanned: 10, next_offset: 10 })
    .mockResolvedValue({ ...empty, total: 27, scanned: 7, next_offset: null })
  render(<MangaDexSearchPage active />)
  fireEvent.change(screen.getByRole('combobox', { name: 'Translation progress' }), {
    target: { value: 'either' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByRole('heading', { name: match.manga[0].title })
  expect(screen.getByText('Page 1 of 3')).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Page 3' }))
  await screen.findByText('Page 3 of 3')
  expect(screen.queryByRole('article')).not.toBeInTheDocument()
  expect(search).toHaveBeenLastCalledWith(expect.objectContaining({ filter: 'either', offset: 20 }))
  expect(screen.getByText(/Comparison pages check 10/)).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Page 1' }))
  expect(screen.getByRole('heading', { name: match.manga[0].title })).toBeVisible()
  expect(search).toHaveBeenCalledTimes(2)
})

it('retains results when optional metadata fails and retries that batch without another search', async () => {
  const search = vi.spyOn(commands, 'searchMangadex').mockResolvedValue(match)
  const metadata = vi
    .spyOn(commands, 'getMangadexMetadata')
    .mockRejectedValueOnce(new Error('Statistics temporarily unavailable'))
    .mockResolvedValue({
      manga: [{ id: match.manga[0].id, rating: null, follows: 0, latest_upload_at: null }],
      checked_at: 1700000000,
      network_requests: 1,
    })
  render(<MangaDexSearchPage active />)
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByText('Could not load ratings, follows or upload dates.')
  expect(screen.getByRole('heading', { name: match.manga[0].title })).toBeVisible()
  expect(screen.getByText(/2018/)).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Retry metadata' }))
  await screen.findByText('0')
  expect(screen.getAllByText('Not available')).toHaveLength(2)
  expect(metadata).toHaveBeenCalledTimes(2)
  expect(search).toHaveBeenCalledTimes(1)
})

it('shows all source tags and opens project creation from the selected manga', async () => {
  const manga = {
    ...match.manga[0],
    tags: Array.from({ length: 10 }, (_, i) => `Source tag ${i + 1}`),
  }
  vi.spyOn(commands, 'searchMangadex').mockResolvedValue({ ...match, manga: [manga] })
  vi.spyOn(commands, 'getSourceImportSettings').mockResolvedValue({
    node: 'node',
    script: '/tool.ts',
    backend: 'codex',
    binary: null,
    model: null,
    catalog_url: '',
    offline: false,
  })
  vi.spyOn(commands, 'inspectPublication').mockResolvedValue({
    node: '/node',
    script: '/tool.ts',
    binary: null,
    missing: [],
    catalog: null,
    catalogError: null,
  })
  const create = vi.spyOn(commands, 'importSourceUrl').mockResolvedValue('queued')
  render(<MangaDexSearchPage active />)
  fireEvent.click(screen.getByRole('button', { name: 'Search' }))
  await screen.findByRole('heading', { name: manga.title })
  expect(screen.getByText('Source tag 10')).toBeVisible()
  fireEvent.click(screen.getByRole('button', { name: 'Create project' }))
  const url = await screen.findByLabelText('Manga URL')
  expect(url).toHaveValue(manga.url)
  expect(screen.getByRole('textbox', { name: 'Project name' })).toHaveValue(`${manga.title} [ko]`)
  expect(create).not.toHaveBeenCalled()
})

it('loads chapter counts only for visible cards and uses the selected languages', async () => {
  const observers: Array<(entries: Array<{ isIntersecting: boolean }>) => void> = []
  class ViewObserver {
    constructor(callback: (typeof observers)[number]) {
      observers.push(callback)
    }
    observe() {}
    disconnect() {}
  }
  vi.stubGlobal('IntersectionObserver', ViewObserver)
  const lookup = vi.spyOn(commands, 'getMangadexChapters').mockResolvedValue({
    reference_language: 'en',
    target_language: 'fr',
    total_available: null,
    latest_available: null,
    missing: 2,
    status: 'behind',
    chapters: [
      { language: 'ko', latest: '12', count: 12, unnumbered: 0 },
      { language: 'en', latest: '10', count: 10, unnumbered: 0 },
      { language: 'fr', latest: '8', count: 8, unnumbered: 0 },
    ],
    checked_at: Math.floor(Date.now() / 1000),
    network_requests: 3,
  })
  vi.spyOn(commands, 'searchMangadex').mockResolvedValue({
    ...match,
    manga: [{ ...match.manga[0], comparison: null }],
  })
  const user = userEvent.setup()
  try {
    render(<MangaDexSearchPage active />)
    fireEvent.change(screen.getByRole('combobox', { name: 'Translation language' }), {
      target: { value: 'fr' },
    })
    await user.click(screen.getByRole('button', { name: 'Search' }))
    await screen.findByRole('article')
    expect(lookup).not.toHaveBeenCalled()
    act(() => observers[0]([{ isIntersecting: true }]))
    await screen.findByText('Ch. 10')
    expect(lookup).toHaveBeenCalledWith(
      expect.objectContaining({ reference: 'en', target: 'fr', original_language: 'ko' }),
    )
    expect(screen.getByText('Ch. 12')).toBeVisible()
    expect(screen.getByText('Ch. 8')).toBeVisible()
    expect(screen.getByRole('img', { name: 'Cover of A Korean series' })).toHaveAttribute(
      'loading',
      'eager',
    )
  } finally {
    vi.unstubAllGlobals()
  }
})

it('serializes visible-card lookups and drops queued work when leaving MangaDex', async () => {
  const observers: Array<(entries: Array<{ isIntersecting: boolean }>) => void> = []
  class ViewObserver {
    constructor(callback: (typeof observers)[number]) {
      observers.push(callback)
    }
    observe() {}
    disconnect() {}
  }
  vi.stubGlobal('IntersectionObserver', ViewObserver)
  let finish!: (value: NonNullable<MangaDexPage['manga'][number]['comparison']>) => void
  const lookup = vi.spyOn(commands, 'getMangadexChapters').mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve
      }),
  )
  vi.spyOn(commands, 'searchMangadex').mockResolvedValue({
    ...match,
    manga: [
      { ...match.manga[0], comparison: null },
      { ...match.manga[0], id: 'second-book', title: 'Second book', comparison: null },
    ],
  })
  try {
    const { rerender } = render(<MangaDexSearchPage active />)
    fireEvent.click(screen.getByRole('button', { name: 'Search' }))
    await screen.findByRole('heading', { name: 'Second book' })
    act(() => observers.forEach((observer) => observer([{ isIntersecting: true }])))
    await waitFor(() => expect(lookup).toHaveBeenCalledTimes(1))
    rerender(<MangaDexSearchPage active={false} />)
    await act(async () => {
      finish(match.manga[0].comparison!)
      await Promise.resolve()
    })
    expect(lookup).toHaveBeenCalledTimes(1)
  } finally {
    vi.unstubAllGlobals()
  }
})

it('does not restart a failed lookup when scrolling the same card out and back into view', async () => {
  let observe!: (entries: Array<{ isIntersecting: boolean }>) => void
  class ViewObserver {
    constructor(callback: typeof observe) {
      observe = callback
    }
    observe() {}
    disconnect() {}
  }
  vi.stubGlobal('IntersectionObserver', ViewObserver)
  const lookup = vi
    .spyOn(commands, 'getMangadexChapters')
    .mockRejectedValue(new Error('Chapter lookup unavailable'))
  vi.spyOn(commands, 'searchMangadex').mockResolvedValue({
    ...match,
    manga: [{ ...match.manga[0], comparison: null }],
  })
  try {
    render(<MangaDexSearchPage active />)
    fireEvent.click(screen.getByRole('button', { name: 'Search' }))
    await screen.findByRole('article')
    await waitFor(() => expect(observe).toBeTypeOf('function'))
    act(() => observe([{ isIntersecting: true }]))
    await screen.findByText('Error: Chapter lookup unavailable')
    await act(async () => {
      observe([{ isIntersecting: false }])
    })
    await act(async () => {
      observe([{ isIntersecting: true }])
    })
    expect(lookup).toHaveBeenCalledTimes(1)
  } finally {
    vi.unstubAllGlobals()
  }
})
