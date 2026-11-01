import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { createElement, StrictMode } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import Providers from '@/app/providers'
import { ChapterPanel } from '@/components/editor/ChapterPanel'
import { call } from '@/lib/backend'
import { chapterKey, queryClient, useProject } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import {
  commands,
  type ChapterDetail,
  type Job,
  type Preferences,
  type ProjectInfo,
  type StartupState,
} from '@koharu/bridge/protocol'

vi.mock('@tauri-apps/api/core', () => ({
  Channel: class<T> {
    onmessage: (payload: T) => void

    constructor(handler: (payload: T) => void) {
      this.onmessage = handler
    }
  },
  invoke: vi.fn(),
}))

const preferences: Preferences = {
  pipeline: {
    detection: { model: 'koharu-layout-rfdetr-seg-2xl' },
    ocr: { model: 'paddleocr-vl-1.6' },
    translation: {
      model: {
        provider: 'local',
        model: 'lfm2.5-1.2b-instruct',
        quantization: null,
        vision: false,
        reasoning: false,
      },
      generation: { vision: true, reasoning: true },
      target_language: 'en-US',
      instructions: null,
    },
    inpainting: { model: 'lama' },
    processor: {},
  },
  providers: {
    entries: [],
  },
  typesetting: {
    font_families: ['CCWildWords', 'Adobe 黑体 Std'],
  },
  languages: [],
}

const project: ProjectInfo = {
  name: 'Book',
  revision: 3,
  active_page: null,
  can_undo: true,
  can_redo: false,
}

beforeEach(() => {
  vi.spyOn(commands, 'getTranslationModels').mockResolvedValue([])
})

const startupState = (): StartupState => ({
  preferences,
  agent: { sequence: 0, conversations: [], run: null },
  jobs: [],
  canvas: {
    page: null,
    revision: null,
    generation: 0,
    size: [0, 0],
    element_frames: [],
  },
})

async function start() {
  const pending = deferred<StartupState>()
  const binding = vi.spyOn(commands, 'subscribe').mockReturnValue(pending.promise)
  const view = render(createElement(Providers, null, createElement('div')))
  pending.resolve(startupState())
  await waitFor(() => expect(useKoharuStore.getState().preferences).toBe(preferences))
  return { binding, dispose: view.unmount }
}

function ProjectProbe() {
  const project = useProject().data
  return createElement(
    'span',
    null,
    project === undefined ? 'Loading' : (project?.name ?? 'Closed'),
  )
}

describe('Tauri runtime', () => {
  it.each(['translate', 'summarize'] as const)(
    'refreshes chapter outputs after summary commits while %s continues',
    async (phase) => {
      let detail: ChapterDetail = {
        id: 'chapter',
        title: 'Chapter 9.5',
        pages: ['page'],
        summary: null,
        setting: null,
        summarize: true,
        wiki: { files: [{ slug: 'characters', body: 'Old wiki' }] },
      }
      vi.spyOn(commands, 'getProject').mockResolvedValue(project)
      vi.spyOn(commands, 'getPage').mockResolvedValue({
        id: 'page',
        label: '001',
        chapter: 'chapter',
        size: { width: 100, height: 160 },
        layers: [],
        regions: [],
      })
      vi.spyOn(commands, 'getPages').mockResolvedValue([])
      vi.spyOn(commands, 'getChapters').mockResolvedValue([
        {
          id: 'chapter',
          title: 'Chapter 9.5',
          pages: ['page'],
          summarize: true,
          has_summary: false,
          has_setting: false,
        },
      ])
      const getChapter = vi.spyOn(commands, 'getChapter').mockImplementation(async () => detail)
      const binding = vi.spyOn(commands, 'subscribe').mockResolvedValue(startupState())
      const view = render(createElement(Providers, null, createElement(ChapterPanel)))
      await screen.findByText(/No summary yet/)
      queryClient.setQueryData(chapterKey('other-chapter'), { wiki: 'Old shared wiki' })
      const [, jobs] = binding.mock.calls[0]
      const job: Job = {
        id: 'chapter-job',
        project: 'Book',
        state: 'running',
        completed: 0,
        total: 62,
        chapter: 'chapter',
        chapter_title: 'Chapter 9.5',
        chapter_progress: null,
        phase: 'summarize',
        page: null,
        stage: null,
        model: null,
        detail: null,
        retry: null,
        error: null,
      }
      act(() => jobs.onmessage(job))
      detail = {
        ...detail,
        summary: 'Committed summary',
        setting: 'Committed prompt',
        wiki: { files: [{ slug: 'characters', body: 'Committed wiki' }] },
      }
      const committed = { ...job, phase, completed: phase === 'summarize' ? 1 : 0 }
      act(() => jobs.onmessage(committed))

      expect(await screen.findByText('Committed summary')).toBeInTheDocument()
      fireEvent.click(screen.getByRole('button', { name: 'Prompt' }))
      expect(await screen.findByText('Committed prompt')).toBeInTheDocument()
      fireEvent.click(screen.getByRole('button', { name: 'Wiki' }))
      expect(await screen.findByText('Committed wiki')).toBeInTheDocument()
      expect(queryClient.getQueryState(chapterKey('other-chapter'))?.isInvalidated).toBe(true)
      expect(useKoharuStore.getState().jobs['chapter-job'].state).toBe('running')
      const reads = getChapter.mock.calls.length
      await act(async () => jobs.onmessage({ ...committed, completed: 1 }))
      expect(getChapter.mock.calls.length).toBe(reads)
      view.unmount()
    },
  )

  it('keeps newer agent events over a late startup snapshot and refreshes only the owning editor', async () => {
    const pending = deferred<StartupState>()
    const binding = vi.spyOn(commands, 'subscribe').mockReturnValue(pending.promise)
    const getProject = vi.spyOn(commands, 'getProject').mockResolvedValue(project)
    const view = render(createElement(Providers, null, createElement(ProjectProbe)))
    await waitFor(() => expect(binding).toHaveBeenCalledTimes(1))
    const agentChannel = binding.mock.calls[0][5]
    const run = {
      id: 'agent',
      project: 'Other',
      state: 'running' as const,
      phase: 'tool' as const,
      tool: 'set_typography',
      page: '007',
      element: null,
      stage: null,
      changes: 1,
      error: null,
    }
    act(() => agentChannel.onmessage({ sequence: 4, conversations: [], run }))
    pending.resolve(startupState())
    expect(await screen.findByText('Book')).toBeInTheDocument()
    expect(useKoharuStore.getState().agent.run?.project).toBe('Other')
    const previous = getProject.mock.calls.length
    act(() =>
      agentChannel.onmessage({ sequence: 5, conversations: [], run: { ...run, changes: 2 } }),
    )
    expect(getProject.mock.calls.length).toBe(previous)
    act(() =>
      agentChannel.onmessage({
        sequence: 6,
        conversations: [],
        run: { ...run, project: 'Book', changes: 3 },
      }),
    )
    await waitFor(() => expect(getProject.mock.calls.length).toBeGreaterThan(previous))
    act(() => agentChannel.onmessage({ sequence: 3, conversations: [], run }))
    expect(useKoharuStore.getState().agent.run?.project).toBe('Book')
    view.unmount()
  })

  it('keeps the project unresolved until its backend query returns', async () => {
    const projectPending = deferred<ProjectInfo | null>()
    vi.spyOn(commands, 'getProject').mockReturnValue(projectPending.promise)
    vi.spyOn(commands, 'subscribe').mockResolvedValue(startupState())
    const view = render(createElement(Providers, null, createElement(ProjectProbe)))

    expect(await screen.findByText('Loading')).toBeInTheDocument()
    projectPending.resolve(null)
    expect(await screen.findByText('Closed')).toBeInTheDocument()
    view.unmount()
  })

  it('keeps one live job channel through Strict Mode effect replay', async () => {
    const binding = vi.spyOn(commands, 'subscribe').mockResolvedValue(startupState())
    const view = render(
      createElement(StrictMode, null, createElement(Providers, null, createElement('div'))),
    )

    await waitFor(() => expect(binding).toHaveBeenCalledTimes(1))
    const [, jobChannel] = binding.mock.calls[0]
    act(() => {
      jobChannel.onmessage({
        id: 'job',
        project: 'Book',
        state: 'running',
        completed: 0,
        total: 4,
        page: 'page',
        chapter: null,
        chapter_title: null,
        chapter_progress: null,
        phase: null,
        detail: null,
        retry: null,
        stage: 'detection',
        model: 'model',
        error: null,
      })
    })

    expect(useKoharuStore.getState().jobs.job).toMatchObject({ state: 'running', total: 4 })
    view.unmount()
  })

  it('passes only domain arguments to mutation commands', async () => {
    const rename = vi.spyOn(commands, 'renamePage').mockResolvedValue(null)

    await expect(call(commands.renamePage, 'page', 'Chapter 1')).resolves.toBeNull()
    expect(rename).toHaveBeenCalledWith('page', 'Chapter 1')
  })

  it('passes the managed project name to open', async () => {
    const open = vi.spyOn(commands, 'openProject').mockResolvedValue(null)
    await expect(call(commands.openProject, 'Volume 1')).resolves.toBeNull()
    expect(open).toHaveBeenCalledWith('Volume 1')
  })

  it('applies independent channel updates directly to the store', async () => {
    useKoharuStore.setState({ downloads: {}, resources: null })
    const { binding, dispose } = await start()
    const [, , downloadChannel, resourcesChannel] = binding.mock.calls[0]

    downloadChannel.onmessage({
      id: 7,
      state: 'running',
      name: 'model.bin',
      completed: 25,
      total: 100,
      error: null,
    })
    resourcesChannel.onmessage({
      process_memory: 1024,
      system_memory: 8192,
      process_cpu: 5,
      devices: [
        { name: 'GPU', selected: true, memory_budget: 8192, memory_used: 4096, utilization: 40 },
      ],
    })
    expect(useKoharuStore.getState().downloads[7]).toMatchObject({ completed: 25, total: 100 })
    expect(useKoharuStore.getState().resources).toMatchObject({
      process_cpu: 5,
      devices: [{ memory_used: 4096 }],
    })
    dispose()
  })

  it('refreshes project queries when an autonomous job commits work', async () => {
    const getProject = vi.spyOn(commands, 'getProject').mockResolvedValue(project)
    const binding = vi.spyOn(commands, 'subscribe').mockResolvedValue(startupState())
    const view = render(createElement(Providers, null, createElement(ProjectProbe)))
    expect(await screen.findByText('Book')).toBeInTheDocument()
    const previousCalls = getProject.mock.calls.length

    const [, jobChannel] = binding.mock.calls[0]
    jobChannel.onmessage({
      id: 'job',
      project: 'Book',
      state: 'running',
      completed: 1,
      total: 2,
      page: 'page',
      chapter: null,
      chapter_title: null,
      chapter_progress: null,
      phase: null,
      detail: null,
      retry: null,
      stage: 'ocr',
      model: 'model',
      error: null,
    })

    await waitFor(() => expect(getProject.mock.calls.length).toBeGreaterThan(previousCalls))
    view.unmount()
  })

  it('keeps background progress out of the active editor cache and clears project data on switching', async () => {
    const { projectKey, pagesKey, pageKey, chaptersKey, chapterKey, queryClient } =
      await import('@/lib/queries')
    const { binding, dispose } = await start()
    queryClient.setQueryData(projectKey, project)
    queryClient.setQueryData(pagesKey, [{ id: 'a-page' }])
    queryClient.setQueryData(pageKey, { id: 'a-page' })
    queryClient.setQueryData(chaptersKey, [{ id: 'a-chapter' }])
    queryClient.setQueryData(chapterKey('a-chapter'), { summary: 'A draft' })
    useKoharuStore.setState({ selectedLayers: ['a-layer'], selectedPages: ['a-page'] })
    const invalidate = vi.spyOn(queryClient, 'invalidateQueries')
    const [, jobChannel, , , projectChannel] = binding.mock.calls[0]
    act(() =>
      jobChannel.onmessage({
        id: 'background',
        project: 'B',
        state: 'running',
        completed: 12,
        total: 20,
        page: 'b-page',
        chapter: null,
        chapter_title: null,
        chapter_progress: null,
        phase: 'summarize',
        detail: null,
        retry: null,
        stage: null,
        model: 'model',
        error: null,
      }),
    )
    act(() =>
      jobChannel.onmessage({ ...useKoharuStore.getState().jobs.background, phase: 'translate' }),
    )
    expect(invalidate).not.toHaveBeenCalled()
    expect(queryClient.getQueryData(pageKey)).toEqual({ id: 'a-page' })
    expect(useKoharuStore.getState().selectedLayers).toEqual(['a-layer'])
    act(() => projectChannel.onmessage({ ...project, name: 'B', active_page: 'b-page' }))
    expect(queryClient.getQueryData(projectKey)).toMatchObject({ name: 'B' })
    expect(queryClient.getQueryData(pageKey)).toBeUndefined()
    expect(queryClient.getQueryData(chapterKey('a-chapter'))).toBeUndefined()
    expect(useKoharuStore.getState().selectedLayers).toEqual([])
    expect(useKoharuStore.getState().selectedPages).toEqual(['b-page'])
    expect(useKoharuStore.getState().jobs.background.project).toBe('B')
    dispose()
  })
})

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise
    reject = rejectPromise
  })
  return { promise, resolve, reject }
}
