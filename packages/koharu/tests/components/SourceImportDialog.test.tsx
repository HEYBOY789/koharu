import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

import { SourceImportDialog } from '@/components/app/SourceImportDialog'
import { useKoharuStore } from '@/lib/store'
import en from '@/public/locales/en-US/translation.json'
import {
  commands,
  type PublicationConfig,
  type PublicationReadiness,
} from '@koharu/bridge/protocol'

afterEach(cleanup)
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
  await waitFor(() => expect(screen.getByText(en.publication.toolsReady)).toBeInTheDocument())
  expect(button).toBeDisabled()
  fireEvent.change(url, {
    target: { value: 'https://books.example/manga/variable-slug' },
  })
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
  await waitFor(() => expect(button).toBeEnabled())
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
  fireEvent.click(button)
  await waitFor(() =>
    expect(start).toHaveBeenCalledWith(url, expect.objectContaining({ node: '/node' }), {
      language: 'ja',
      download_url: 'https://books.example/manga/test',
      project_name: 'Manga test [ja]',
      summarize: true,
      translate: true,
    }),
  )
  expect(useKoharuStore.getState().jobs.working).toEqual(busyJob)
})
