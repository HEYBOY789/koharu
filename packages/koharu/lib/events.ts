'use client'

import type { QueryKey } from '@tanstack/react-query'

import { commands, events, type Change, type PageId } from '@koharu/ipc/protocol'
import { toast } from '@koharu/ui/components/toast'

import { toHex } from './color'
import i18n from './i18n'
import {
  chapterJobsKey,
  pageKey,
  pagesKey,
  projectKey,
  projectMetadataKey,
  projectsKey,
  queryClient,
  refresh,
  sourceJobsKey,
  type ProjectView,
} from './queries'
import {
  receiveCanvasSelection,
  receiveDownload,
  receivePickedColor,
  receivePipeline,
  receiveRuns,
  useKoharuStore,
} from './store'

/** Hears what the desktop tells for the interface's life, while `active` holds, which it doesn't
 * while React replays an effect: listening first, then reading the runs so far, so nothing told
 * while they load is missed. */
export function hear(active: () => boolean): Promise<void> {
  const listen = <T>(
    event: (handler: (payload: T) => void) => Promise<unknown>,
    receive: (payload: T) => void,
  ) =>
    event((payload) => {
      if (active()) receive(payload)
    })

  return Promise.all([
    listen(events.change, receiveChange),
    listen(events.project, receiveProject),
    listen(events.page, receivePage),
    listen(events.selection, receiveCanvasSelection),
    listen(events.camera, (camera) => useKoharuStore.setState({ camera })),
    listen(events.selectionBounds, (selectionBounds) =>
      useKoharuStore.setState({ selectionBounds }),
    ),
    listen(events.area, (area) => useKoharuStore.setState({ area })),
    listen(events.typing, (typing) => useKoharuStore.setState({ typing })),
    listen(events.color, (color) => receivePickedColor(toHex(color))),
    listen(events.brushDiameter, (diameter) => {
      const store = useKoharuStore.getState()
      store.setBrush({ ...store.brush, diameter })
    }),
    listen(events.notice, (notice) =>
      toast.add({ type: 'info', title: i18n.t(`canvas.notice.${notice}`) }),
    ),
    listen(events.pipeline, receivePipeline),
    listen(events.download, receiveDownload),
    listen(events.chapterJob, () => void refresh(chapterJobsKey).catch(() => undefined)),
    listen(events.sourceJob, (job) => {
      const keys = job.phase === 'finished' ? [sourceJobsKey, projectsKey] : [sourceJobsKey]
      void refresh(...keys).catch(() => undefined)
    }),
  ])
    .then(() => commands.getPipelineEvents())
    .then((runs) => {
      if (active()) receiveRuns(runs)
    })
}

/** Another project opened, or the open one closed: its selection starts empty. A project told
 * again, after changes the interface missed, is read again. */
function receiveProject(name: string | null): void {
  const previous = queryClient.getQueryData<ProjectView | null>(projectKey)
  if (previous?.name !== name) {
    const store = useKoharuStore.getState()
    store.selectPages([])
    store.selectLayers([])
  }
  if (name === null) {
    queryClient.setQueryData(projectKey, null)
    queryClient.setQueryData(pageKey, null)
  } else {
    void refresh(projectKey, pageKey, ['project-metadata']).catch(() => undefined)
  }
}

/** The page the canvas shows. The project is read again too: a read already under way, which a
 * change starts, answers the page shown before, and would land over this one. */
function receivePage(page: PageId | null): void {
  queryClient.setQueryData<ProjectView | null>(projectKey, (project) =>
    project ? { ...project, page } : project,
  )
  if (page !== null && useKoharuStore.getState().selectedPages.length === 0) {
    useKoharuStore.getState().selectPages([page])
  }
  void refresh(projectKey, pageKey).catch(() => undefined)
}

/** Layer edits update only the touched pages. Structural edits reload page order. */
function receiveChange(change: Change): void {
  const current = queryClient.getQueryData<ProjectView | null>(projectKey)
  if (!current || current.id !== change.project) return
  const summaries = new Map(change.pages.summaries.map((page) => [page.id, page]))
  queryClient.setQueryData<ProjectView | null>(projectKey, (project) =>
    project
      ? {
          ...project,
          history: change.history,
          pages: summaries.size
            ? project.pages.map((page) => summaries.get(page.id) ?? page)
            : project.pages,
          revisions:
            change.pages.changed.length > 0
              ? {
                  ...project.revisions,
                  ...Object.fromEntries(
                    change.pages.changed.map((page) => [page, change.revision]),
                  ),
                }
              : project.revisions,
        }
      : project,
  )
  const keys: QueryKey[] = []
  if (change.metadata_changed) keys.push(projectMetadataKey(change.project))
  if (
    change.pages.added.length ||
    change.pages.removed.length ||
    change.pages.reordered ||
    queryClient.isFetching({ queryKey: projectKey })
  ) {
    keys.push(projectKey)
  }
  if (
    current.page &&
    (change.pages.changed.includes(current.page) || change.pages.removed.includes(current.page))
  ) {
    keys.push(pageKey)
  }
  keys.push(...change.pages.changed.map((page) => [...pagesKey, page]))
  if (keys.length > 0) void refresh(...keys).catch(() => undefined)
}
