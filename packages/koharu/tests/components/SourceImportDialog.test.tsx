import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'

import { SourceImportDialog } from '@/components/app/SourceImportDialog'
import { useKoharuStore } from '@/lib/store'
import en from '@/public/locales/en-US/translation.json'
import {
  commands,
  type PublicationConfig,
  type PublicationReadiness,
} from '@koharu/bridge/protocol'

afterEach(cleanup)
const chapterURLs = ['https://books.example/chapter-1', 'https://books.example/chapter-2.5']
beforeEach(() => {
  vi.spyOn(commands, 'previewSourceUrl').mockImplementation(async (url, _config, options) => ({
    title: 'Preview book',
    source_url: options.download_url || url,
    chapters: chapterURLs.map((url, index) => ({
      url,
      number: index === 0 ? 1 : 2.5,
      title: index === 0 ? 'Chapter 1' : 'Chapter 2.5',
    })),
  }))
})
const config: PublicationConfig = {
  node: 'node',
  script: '/publisher/scripts/publishing/main.ts',
  catalog_url: '',
  backend: 'codex',
  binary: null,
  model: null,
  offline: false,
}
const ready: PublicationReadiness = {
  node: '/node',
  script: config.script ?? null,
  binary: '/codex',
  missing: [],
  catalog: null,
  catalogError: 'No website catalog',
}

it('imports with resolved local tools without requiring a website catalog, and rejects a non-web URL', async () => {
  vi.spyOn(commands, 'getSourceImportSettings').mockResolvedValue(config)
  vi.spyOn(commands, 'inspectPublication').mockResolvedValue(ready)
  const start = vi.spyOn(commands, 'importSourceUrl').mockResolvedValue('job')
  const close = vi.fn()
  render(<SourceImportDialog open onOpenChange={close} />)
  const url = await screen.findByLabelText(en.sourceImport.url)
  const button = screen.getByRole('button', { name: en.sourceImport.start })
  fireEvent.change(url, { target: { value: 'file:///private' } })
  fireEvent.blur(url)
  await waitFor(() => expect(screen.getByText(en.publication.toolsReady)).toBeInTheDocument())
  expect(button).toBeDisabled()
  expect(url).toHaveAttribute('aria-invalid', 'true')
  expect(document.getElementById(url.getAttribute('aria-describedby')!)).toHaveTextContent(
    'https://',
  )
  fireEvent.change(url, {
    target: { value: 'https://books.example/manga/variable-slug' },
  })
  await waitFor(() =>
    expect(screen.getByRole('button', { name: en.sourceImport.preview })).toBeEnabled(),
  )
  expect(button).toBeDisabled()
  fireEvent.click(screen.getByRole('button', { name: en.sourceImport.preview }))
  await waitFor(() => expect(button).toBeEnabled())
  fireEvent.click(button)
  await waitFor(() =>
    expect(start).toHaveBeenCalledWith(
      'https://books.example/manga/variable-slug',
      expect.objectContaining({ node: '/node', binary: '/codex' }),
      {
        language: null,
        download_url: null,
        project_name: null,
        summarize: false,
        translate: false,
        chapters: chapterURLs,
      },
    ),
  )
  expect(close).toHaveBeenCalledWith(false)
})

it('ignores tool discovery that completes after the configured executable changes', async () => {
  vi.spyOn(commands, 'getSourceImportSettings').mockResolvedValue(config)
  let finish!: (value: PublicationReadiness) => void
  vi.spyOn(commands, 'inspectPublication').mockImplementation((value) =>
    value.node === 'node'
      ? new Promise((resolve) => {
          finish = resolve
        })
      : Promise.resolve({ ...ready, node: null, missing: ['node'] }),
  )
  render(<SourceImportDialog open onOpenChange={() => {}} />)
  await waitFor(() => expect(finish).toBeDefined())
  fireEvent.change(screen.getByLabelText(en.publication.node), {
    target: { value: '/missing-node' },
  })
  await waitFor(() =>
    expect(
      screen.getByText(en.publication.toolsMissing.replace('{{tools}}', en.publication.node)),
    ).toBeInTheDocument(),
  )
  await act(async () => finish(ready))
  expect(screen.getByRole('button', { name: en.sourceImport.start })).toBeDisabled()
  expect(screen.queryByText(en.publication.toolsReady)).not.toBeInTheDocument()
})

it('binds a decimal chapter range to the preview and rejects it after source changes', async () => {
  vi.spyOn(commands, 'getSourceImportSettings').mockResolvedValue(config)
  vi.spyOn(commands, 'inspectPublication').mockResolvedValue(ready)
  const start = vi.spyOn(commands, 'importSourceUrl').mockResolvedValue('job')
  render(<SourceImportDialog open onOpenChange={() => {}} />)
  const url = await screen.findByLabelText(en.sourceImport.url)
  fireEvent.change(url, { target: { value: 'https://books.example/book' } })
  const preview = screen.getByRole('button', { name: en.sourceImport.preview })
  await waitFor(() => expect(preview).toBeEnabled())
  fireEvent.click(preview)
  const scope = await screen.findByRole('combobox', { name: en.sourceImport.chapterSelection })
  fireEvent.change(scope, { target: { value: 'range' } })
  fireEvent.change(screen.getByLabelText(en.sourceImport.chapterFrom), { target: { value: '2.5' } })
  fireEvent.change(screen.getByLabelText(en.sourceImport.chapterTo), { target: { value: '2.5' } })
  fireEvent.click(screen.getByRole('button', { name: en.sourceImport.start }))
  await waitFor(() =>
    expect(start).toHaveBeenCalledWith(
      expect.any(String),
      expect.anything(),
      expect.objectContaining({ chapters: [chapterURLs[1]] }),
    ),
  )
  fireEvent.change(url, { target: { value: 'https://books.example/different-book' } })
  expect(screen.getByRole('button', { name: en.sourceImport.start })).toBeDisabled()
})

it('queues a MangaDex project with chosen language and automatic workflow using a script source and the configured agent', async () => {
  vi.spyOn(commands, 'getSourceImportSettings').mockResolvedValue(config)
  const inspectAgent = vi.spyOn(commands, 'inspectPublication').mockResolvedValue(ready)
  const start = vi.spyOn(commands, 'importSourceUrl').mockResolvedValue('queued')
  const busyJob = {
    id: 'working',
    project: 'Other project',
    state: 'running' as const,
    completed: 2,
    total: 5,
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
  useKoharuStore.setState({ jobs: { working: busyJob } })
  const url = 'https://mangadex.org/title/00000000-0000-0000-0000-000000000001'
  render(
    <SourceImportDialog
      open
      onOpenChange={() => {}}
      manga={{ title: 'Manga test', url, language: 'en', tags: ['Drama', 'Horror'] }}
    />,
  )
  const button = await screen.findByRole('button', { name: en.sourceImport.start })
  await waitFor(() =>
    expect(screen.getByRole('button', { name: en.sourceImport.preview })).toBeEnabled(),
  )
  expect(inspectAgent).toHaveBeenCalledWith(config, false)
  expect(screen.getByRole('checkbox', { name: en.sourceImport.autoSummarize })).not.toBeChecked()
  expect(screen.getByRole('checkbox', { name: en.sourceImport.autoTranslate })).not.toBeChecked()
  fireEvent.change(screen.getByRole('combobox', { name: en.sourceImport.downloadLanguage }), {
    target: { value: 'ja' },
  })
  expect(screen.getByRole('textbox', { name: en.sourceImport.projectName })).toHaveValue(
    'Manga test [ja]',
  )
  fireEvent.change(screen.getByRole('textbox', { name: en.sourceImport.downloadURL }), {
    target: { value: 'https://books.example/manga/test' },
  })
  fireEvent.click(screen.getByRole('checkbox', { name: en.sourceImport.autoSummarize }))
  fireEvent.click(screen.getByRole('checkbox', { name: en.sourceImport.autoTranslate }))
  fireEvent.click(screen.getByRole('button', { name: en.sourceImport.preview }))
  await screen.findByRole('button', { name: en.sourceImport.firstChapter })
  fireEvent.click(screen.getByRole('button', { name: en.sourceImport.firstChapter }))
  fireEvent.click(button)
  await waitFor(() =>
    expect(start).toHaveBeenCalledWith(url, expect.objectContaining({ node: '/node' }), {
      language: 'ja',
      download_url: 'https://books.example/manga/test',
      project_name: 'Manga test [ja]',
      summarize: true,
      translate: true,
      chapters: [chapterURLs[0]],
    }),
  )
  expect(useKoharuStore.getState().jobs.working).toEqual(busyJob)
})
