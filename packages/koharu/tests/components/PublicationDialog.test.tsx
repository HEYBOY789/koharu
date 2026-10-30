import { act, fireEvent, render, screen, waitFor, cleanup } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import {
  commands,
  type PublicationReadiness,
  type PublicationSettings,
} from '@koharu/bridge/protocol'
import { PublicationDialog } from '@/components/app/PublicationDialog'
import en from '@/public/locales/en-US/translation.json'

const text = en.publication
const originalURL = 'https://one.example/catalog.json'
const settings: PublicationSettings = {
  metadata: {
    title: 'Test',
    slug: 'test',
    author: '',
    alternativeTitles: '',
    releaseYear: null,
    country: '',
    status: 'ongoing',
    genres: [],
  },
  config: {
    node: 'node',
    script: '/publisher/scripts/publishing/main.ts',
    catalog_url: originalURL,
    backend: 'codex',
    binary: null,
    model: null,
    offline: false,
  },
}
const ready: PublicationReadiness = {
  node: '/node',
  script: settings.config.script || null,
  binary: '/codex',
  missing: [],
  catalog: null,
  catalogError: null,
}

beforeEach(() => {
  vi.spyOn(commands, 'getPublicationSettings').mockResolvedValue(structuredClone(settings))
})
afterEach(cleanup)

it('requires a matching saved catalog for offline mode and invalidates it immediately when the website changes', async () => {
  const inspect = vi
    .spyOn(commands, 'inspectPublication')
    .mockImplementation(async (config, refresh) => ({
      ...ready,
      catalog: refresh
        ? {
            url: config.catalog_url || '',
            siteUrl: new URL(config.catalog_url || originalURL).origin,
            storyCount: 2,
            downloadedAt: '2026-10-26T10:00:00Z',
          }
        : null,
    }))
  render(<PublicationDialog open onOpenChange={() => {}} />)
  const checkbox = await screen.findByRole('checkbox', { name: text.offline })
  await waitFor(() => expect(inspect).toHaveBeenCalled())
  expect(checkbox).toHaveAttribute('aria-disabled', 'true')
  const update = screen.getByRole('button', { name: text.refreshCatalog })
  await waitFor(() => expect(update).toBeEnabled())
  fireEvent.click(update)
  await waitFor(() => expect(checkbox).not.toHaveAttribute('aria-disabled', 'true'))
  fireEvent.click(checkbox)
  expect(checkbox).toBeChecked()
  fireEvent.change(screen.getByLabelText(text.catalog), {
    target: { value: 'https://two.example/catalog.json' },
  })
  expect(checkbox).toHaveAttribute('aria-disabled', 'true')
  expect(checkbox).not.toBeChecked()
  await waitFor(() =>
    expect(inspect).toHaveBeenLastCalledWith(
      expect.objectContaining({ catalog_url: 'https://two.example/catalog.json', offline: false }),
      false,
    ),
  )
})

it('ignores a late inspection from a previous website', async () => {
  let finishOld!: (value: PublicationReadiness) => void
  vi.spyOn(commands, 'inspectPublication').mockImplementation((config) => {
    if (config.catalog_url === settings.config.catalog_url)
      return new Promise((resolve) => {
        finishOld = resolve
      })
    return Promise.resolve({ ...ready, catalog: null })
  })
  render(<PublicationDialog open onOpenChange={() => {}} />)
  await waitFor(() => expect(finishOld).toBeDefined())
  fireEvent.change(screen.getByLabelText(text.catalog), {
    target: { value: 'https://new.example/catalog.json' },
  })
  await waitFor(() => expect(screen.getByRole('button', { name: text.prepare })).toBeEnabled())
  await act(async () =>
    finishOld({
      ...ready,
      catalog: {
        url: originalURL,
        siteUrl: 'https://one.example',
        storyCount: 9,
        downloadedAt: '2026-10-26T10:00:00Z',
      },
    }),
  )
  expect(screen.getByRole('checkbox', { name: text.offline })).toHaveAttribute(
    'aria-disabled',
    'true',
  )
  expect(screen.queryByText('https://one.example')).not.toBeInTheDocument()
})
