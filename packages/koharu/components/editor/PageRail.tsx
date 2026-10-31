'use client'

import { useQuery } from '@tanstack/react-query'
import { observeElementRect, useVirtualizer } from '@tanstack/react-virtual'
import {
  BookOpen,
  ChevronRight,
  FilePlus2,
  FolderOpen,
  LoaderCircle,
  MoreHorizontal,
  Plus,
  Search,
  Settings,
  Sparkles,
  Trash2,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { ResourceMonitor } from '@/components/editor/ResourceMonitor'
import { call } from '@/lib/backend'
import {
  chaptersKey,
  pageKey,
  pagesKey,
  preparedPageKey,
  projectKey,
  queryClient,
  refresh,
  useChapters,
  useImportPages,
  usePage,
  usePages,
  useProjectBusy,
} from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { prefetchCanvasPages, showCanvasPage } from '@koharu/bridge/canvas'
import {
  commands,
  type CanvasPagePreparation,
  type ChapterInfo,
  type Page,
  type PageImportSource,
  type PageSummary,
  type ProjectInfo,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@koharu/ui/components/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@koharu/ui/components/dropdown-menu'
import { Input } from '@koharu/ui/components/input'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Tooltip, TooltipContent, TooltipTrigger } from '@koharu/ui/components/tooltip'
import { cn } from '@koharu/ui/lib/utils'

const emptyPages: PageSummary[] = []
const emptyChapters: ChapterInfo[] = []

interface IntentPrefetchState {
  project: string
  revision: number
  pages: Set<string>
}

type Row =
  | { kind: 'chapter'; chapter: ChapterInfo }
  | { kind: 'empty'; chapter: ChapterInfo }
  | { kind: 'page'; page: PageSummary; index: number }

export function PageRail() {
  const { t } = useTranslation()
  const pages = usePages().data ?? emptyPages
  const chapters = useChapters().data ?? emptyChapters
  const active = usePage().data?.id ?? null
  const selected = useKoharuStore((state) => state.selectedPages)
  const selectPages = useKoharuStore((state) => state.selectPages)
  const selectLayers = useKoharuStore((state) => state.selectLayers)
  const setSettingsOpen = useKoharuStore((state) => state.setSettingsOpen)
  const setInspectorPanel = useKoharuStore((state) => state.setInspectorPanel)
  const busy = useProjectBusy()
  const { importPages, importing } = useImportPages()
  const anchor = useRef<number | null>(null)
  const selectionRequest = useRef(0)
  const pendingFocus = useRef<string | null>(null)
  const intentPrefetch = useRef<IntentPrefetchState | null>(null)
  const [dragged, setDragged] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  const [renaming, setRenaming] = useState<PageSummary | null>(null)
  const [renameValue, setRenameValue] = useState('')
  const [chapterDialog, setChapterDialog] = useState<
    | { mode: 'create' }
    | { mode: 'rename'; chapter: ChapterInfo }
    | { mode: 'delete'; chapter: ChapterInfo }
    | null
  >(null)
  const [chapterTitle, setChapterTitle] = useState('')
  const normalized = query.trim().toLocaleLowerCase()

  const activeChapter = chapters.find((chapter) => chapter.pages.includes(active ?? ''))?.id ?? null
  useEffect(() => {
    if (!activeChapter) return
    setCollapsed((current) => {
      if (!current.has(activeChapter)) return current
      const next = new Set(current)
      next.delete(activeChapter)
      return next
    })
  }, [activeChapter])

  const rows = useMemo(() => {
    const result: Row[] = []
    const assigned = new Set<string>()
    for (const chapter of chapters) {
      const matching = pages
        .map((page, index) => ({ page, index }))
        .filter(({ page }) => chapter.pages.includes(page.id))
        .filter(({ page }) => !normalized || page.label.toLocaleLowerCase().includes(normalized))
      for (const { page } of matching) assigned.add(page.id)
      if (matching.length === 0 && (normalized || chapter.pages.length > 0)) continue
      result.push({ kind: 'chapter', chapter })
      if (normalized || !collapsed.has(chapter.id)) {
        for (const item of matching) result.push({ kind: 'page', ...item })
        if (matching.length === 0) result.push({ kind: 'empty', chapter })
      }
    }
    const loose = pages
      .map((page, index) => ({ page, index }))
      .filter(({ page }) => !assigned.has(page.id))
      .filter(({ page }) => !normalized || page.label.toLocaleLowerCase().includes(normalized))
    for (const item of loose) result.push({ kind: 'page', ...item })
    return result
  }, [chapters, collapsed, normalized, pages])
  const pageList = useRef<HTMLDivElement>(null)
  const pageVirtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => pageList.current,
    getItemKey: (index) => {
      const row = rows[index]
      if (!row) return index
      if (row.kind === 'page') return row.page.id
      return `${row.kind}:${row.chapter.id}`
    },
    estimateSize: (index) => (rows[index]?.kind === 'page' ? 72 : 32),
    measureElement: (element) => element.getBoundingClientRect().height,
    gap: 2,
    overscan: 6,
    initialRect: { width: 240, height: 600 },
    observeElementRect: (instance, callback) =>
      observeElementRect(instance, (rect) =>
        callback({ width: rect.width || 240, height: rect.height || 600 }),
      ),
  })
  const virtualRows = pageVirtualizer.getVirtualItems()
  const renderedPages = virtualRows
    .map((row) => rows[row.index])
    .filter((row) => row.kind === 'page')
  const tabbablePage = renderedPages.some((row) => row.page.id === active)
    ? active
    : renderedPages[0]?.page.id

  useEffect(
    () => () => {
      selectionRequest.current += 1
    },
    [],
  )

  const navigatePage = (id: string, direction: 'next' | 'previous' | 'first' | 'last') => {
    const visible = rows.filter((row) => row.kind === 'page')
    const current = visible.findIndex((row) => row.page.id === id)
    const index =
      direction === 'first'
        ? 0
        : direction === 'last'
          ? visible.length - 1
          : Math.max(0, Math.min(visible.length - 1, current + (direction === 'next' ? 1 : -1)))
    const target = visible[index]
    if (!target) return
    pendingFocus.current = target.page.id
    pageVirtualizer.scrollToIndex(rows.indexOf(target))
    const button = Array.from(
      pageList.current?.querySelectorAll<HTMLButtonElement>('[data-page-open]') ?? [],
    ).find((node) => node.dataset.pageOpen === target.page.id)
    if (button) {
      pendingFocus.current = null
      button.focus()
    }
  }

  const select = (index: number, additive: boolean, range: boolean) => {
    const page = pages[index]
    if (!page) return
    let next: string[]
    if (range && anchor.current !== null) {
      const start = Math.min(anchor.current, index)
      const end = Math.max(anchor.current, index)
      const rangeIds = pages.slice(start, end + 1).map((item) => item.id)
      next = additive ? [...new Set([...selected, ...rangeIds])] : rangeIds
    } else if (additive) {
      next = selected.includes(page.id)
        ? selected.filter((id) => id !== page.id)
        : [...selected, page.id]
      anchor.current = index
    } else {
      next = [page.id]
      anchor.current = index
    }
    const previousProject = queryClient.getQueryData<ProjectInfo | null>(projectKey)
    const previousPage = queryClient.getQueryData<Page | null>(pageKey)
    const prepared = queryClient.getQueryData<CanvasPagePreparation>(preparedPageKey(page.id))
    const activated = showCanvasPage(page.id, previousProject?.revision ?? null)
    const request = ++selectionRequest.current
    const synchronize = () => {
      if (selectionRequest.current !== request) return
      if (activated && previousProject && prepared?.revision === previousProject.revision) {
        queryClient.setQueryData(projectKey, { ...previousProject, active_page: page.id })
        queryClient.setQueryData(pageKey, prepared.page)
      }
      selectPages(next)
      selectLayers([])
      void call(commands.selectPage, page.id)
        .then((selection) => {
          if (selectionRequest.current !== request) return
          queryClient.setQueryData(projectKey, selection.project)
          queryClient.setQueryData(pageKey, selection.page)
        })
        .catch(() => {
          if (selectionRequest.current !== request) return
          if (queryClient.getQueryData<ProjectInfo | null>(projectKey)?.active_page === page.id) {
            queryClient.setQueryData(projectKey, previousProject)
            queryClient.setQueryData(pageKey, previousPage)
          }
        })
    }
    if (activated) {
      requestAnimationFrame(() => window.setTimeout(synchronize, 0))
    } else {
      synchronize()
    }
  }

  const prefetchOnIntent = (page: string) => {
    const project = queryClient.getQueryData<ProjectInfo | null>(projectKey)
    if (!project || project.active_page === page) return
    let state = intentPrefetch.current
    if (!state || state.project !== project.name || state.revision !== project.revision) {
      state = { project: project.name, revision: project.revision, pages: new Set() }
      intentPrefetch.current = state
    }
    const prepared = queryClient.getQueryData<CanvasPagePreparation>(preparedPageKey(page))
    if (prepared?.revision === project.revision || state.pages.has(page)) return
    state.pages.add(page)
    void prefetchCanvasPages([page])
      .then((preparedPages) => {
        const current = queryClient.getQueryData<ProjectInfo | null>(projectKey)
        const preparedPage = preparedPages.find(
          (candidate) => candidate.page.id === page && candidate.revision === project.revision,
        )
        if (
          preparedPage &&
          current?.name === project.name &&
          current.revision === preparedPage.revision
        ) {
          queryClient.setQueryData(preparedPageKey(page), preparedPage)
          return
        }
        if (intentPrefetch.current === state) state.pages.delete(page)
      })
      .catch(() => {
        if (intentPrefetch.current === state) state.pages.delete(page)
      })
  }

  const deletePage = (page: string) =>
    void call(commands.deletePages, [page])
      .then(() => {
        selectPages(selected.filter((selectedPage) => selectedPage !== page))
        if (active === page) selectLayers([])
        return refresh(projectKey, pagesKey, pageKey, chaptersKey)
      })
      .catch(() => undefined)

  const openRename = (page: PageSummary) => {
    setRenaming(page)
    setRenameValue(page.label)
  }

  const closeRename = () => {
    setRenaming(null)
    setRenameValue('')
  }

  const submitRename = () => {
    const page = renaming
    const label = renameValue.trim()
    if (!page || !label || label === page.label) {
      closeRename()
      return
    }
    void call(commands.renamePage, page.id, label)
      .then(() => refresh(projectKey, pagesKey, pageKey))
      .catch(() => undefined)
      .finally(closeRename)
  }

  const openChapter = (chapter: ChapterInfo) => {
    const first = pages.findIndex((page) => page.id === chapter.pages[0])
    if (first >= 0) select(first, false, false)
    setInspectorPanel('chapter')
  }

  const movePage = (index: number) => {
    if (!dragged) return
    const current = pages.findIndex((page) => page.id === dragged)
    if (current !== index) {
      void call(commands.movePage, dragged, index)
        .then(() => refresh(projectKey, pagesKey, chaptersKey, pageKey))
        .catch(() => undefined)
    }
    setDragged(null)
  }

  const submitChapter = () => {
    if (!chapterDialog) return
    let request: Promise<unknown> | null = null
    if (chapterDialog.mode === 'delete') {
      request = call(commands.deleteChapter, chapterDialog.chapter.id)
    } else {
      const title = chapterTitle.trim()
      if (!title) return
      request =
        chapterDialog.mode === 'create'
          ? call(commands.createChapter, title)
          : call(commands.renameChapter, chapterDialog.chapter.id, title)
    }
    void request
      .then(() => {
        setChapterDialog(null)
        setChapterTitle('')
        return refresh(projectKey, pagesKey, pageKey, chaptersKey)
      })
      .catch(() => undefined)
  }

  return (
    <>
      <aside className='flex h-full min-h-0 flex-col bg-[var(--surface-sidebar)]'>
        <header className='flex h-10 shrink-0 items-center px-2.5'>
          <div className='flex min-w-0 items-center gap-2'>
            <h2 className='text-[11px] font-semibold'>{t('navigator.pages')}</h2>
            <span className='rounded-full bg-primary/[0.07] px-1.5 py-0.5 text-[9px] text-muted-foreground tabular-nums'>
              {pages.length}
            </span>
          </div>
          {selected.length > 1 && (
            <span
              role='status'
              aria-live='polite'
              className='ml-auto text-[9px] text-muted-foreground tabular-nums'
            >
              {t('navigator.selected', { count: selected.length })}
            </span>
          )}
          <Button
            type='button'
            variant='ghost'
            size='icon-xs'
            className='ml-auto size-6'
            aria-label={t('chapter.new')}
            onClick={() => {
              setChapterTitle('')
              setChapterDialog({ mode: 'create' })
            }}
          >
            <Plus className='size-3.5' />
          </Button>
        </header>

        {pages.length > 0 && (
          <div className='border-b px-2 py-1.5'>
            <label className='flex h-6 items-center gap-1.5 rounded-md border border-input bg-background/70 px-1.5'>
              <Search className='size-3 text-muted-foreground' />
              <Input
                value={query}
                aria-label={t('navigator.filter')}
                placeholder={t('navigator.filter')}
                className='h-5 min-w-0 border-0 bg-transparent p-0 text-[11px] shadow-none focus-visible:ring-0 md:text-[11px]'
                onChange={(event) => setQuery(event.currentTarget.value)}
              />
            </label>
          </div>
        )}

        {pages.length === 0 ? (
          <div className='flex min-h-0 flex-1 flex-col items-center justify-center px-4 text-center'>
            <p className='text-[11px] font-medium'>{t('navigator.emptyTitle')}</p>
            <p className='mt-1 text-[10px] leading-4 text-muted-foreground'>
              {t('navigator.emptyDescription')}
            </p>
            <p className='mt-2 text-[9px] leading-4 text-muted-foreground'>
              {t('navigator.folderChapterHint')}
            </p>
            <div className='mt-3'>
              <PageImportMenu
                importing={importing}
                onImport={(source) => importPages(source, null)}
              />
            </div>
          </div>
        ) : rows.length === 0 ? (
          <div className='grid flex-1 place-items-center px-4 text-center text-[10px] text-muted-foreground'>
            {t('navigator.noResults')}
          </div>
        ) : (
          <ScrollArea
            className='min-h-0 flex-1'
            viewportClassName='px-1.5 py-1.5'
            viewportRef={pageList}
          >
            <div className='relative w-full' style={{ height: pageVirtualizer.getTotalSize() }}>
              {virtualRows.map((virtualRow) => {
                const row = rows[virtualRow.index]
                if (!row) return null
                return (
                  <div
                    key={virtualRow.key}
                    data-index={virtualRow.index}
                    ref={pageVirtualizer.measureElement}
                    className='absolute top-0 left-0 w-full'
                    style={{ transform: `translateY(${virtualRow.start}px)` }}
                  >
                    {row.kind === 'chapter' ? (
                      <ChapterHeader
                        chapter={row.chapter}
                        index={chapters.findIndex((chapter) => chapter.id === row.chapter.id)}
                        last={chapters.at(-1)?.id === row.chapter.id}
                        active={row.chapter.id === activeChapter}
                        collapsed={collapsed.has(row.chapter.id) && !normalized}
                        onOpen={() => openChapter(row.chapter)}
                        onToggle={() =>
                          setCollapsed((current) => {
                            const next = new Set(current)
                            if (next.has(row.chapter.id)) next.delete(row.chapter.id)
                            else next.add(row.chapter.id)
                            return next
                          })
                        }
                        onRename={() => {
                          setChapterTitle(row.chapter.title)
                          setChapterDialog({ mode: 'rename', chapter: row.chapter })
                        }}
                        onMove={(offset) => {
                          const index = chapters.findIndex(
                            (chapter) => chapter.id === row.chapter.id,
                          )
                          if (index < 0) return
                          void call(commands.moveChapter, row.chapter.id, index + offset)
                            .then(() => refresh(projectKey, chaptersKey))
                            .catch(() => undefined)
                        }}
                        onDelete={() => setChapterDialog({ mode: 'delete', chapter: row.chapter })}
                        importing={importing}
                        busy={busy}
                        onImport={(source) => importPages(source, row.chapter.id)}
                        onDrop={() => {
                          const first = pages.findIndex((page) => page.id === row.chapter.pages[0])
                          if (first >= 0) movePage(first)
                          else if (dragged) {
                            void call(commands.assignPage, dragged, row.chapter.id)
                              .then(() => refresh(projectKey, pagesKey, chaptersKey, pageKey))
                              .catch(() => undefined)
                              .finally(() => setDragged(null))
                          }
                        }}
                      />
                    ) : row.kind === 'empty' ? (
                      <EmptyChapterRow
                        importing={importing}
                        onImport={(source) => importPages(source, row.chapter.id)}
                      />
                    ) : (
                      <PageItem
                        page={row.page}
                        active={active === row.page.id}
                        selected={selected.includes(row.page.id)}
                        dragged={dragged === row.page.id}
                        nested={Boolean(row.page.chapter)}
                        tabIndex={tabbablePage === row.page.id ? 0 : -1}
                        buttonRef={(node) => {
                          if (node && pendingFocus.current === row.page.id) {
                            pendingFocus.current = null
                            node.focus()
                          }
                        }}
                        onNavigate={(direction) => navigatePage(row.page.id, direction)}
                        onIntent={
                          active === row.page.id ? undefined : () => prefetchOnIntent(row.page.id)
                        }
                        onSelect={(additive, range) => select(row.index, additive, range)}
                        onDragStart={() => setDragged(row.page.id)}
                        onDragEnd={() => setDragged(null)}
                        onRename={() => openRename(row.page)}
                        onDelete={() => deletePage(row.page.id)}
                        onDrop={() => movePage(row.index)}
                      />
                    )}
                  </div>
                )
              })}
            </div>
          </ScrollArea>
        )}

        <div className='mt-auto flex shrink-0 items-center gap-2 px-2 py-2'>
          <Tooltip>
            <TooltipTrigger
              render={
                <Button
                  type='button'
                  variant='ghost'
                  size='icon'
                  className='size-8 text-muted-foreground hover:bg-foreground/[0.06] hover:text-foreground'
                  aria-label={t('menu.settings')}
                  onClick={() => setSettingsOpen(true)}
                />
              }
            >
              <Settings className='size-4' />
            </TooltipTrigger>
            <TooltipContent side='right'>{t('menu.settings')}</TooltipContent>
          </Tooltip>
          <ResourceMonitor />
        </div>
      </aside>

      <Dialog
        open={renaming !== null}
        onOpenChange={(open) => {
          if (!open) closeRename()
        }}
      >
        <DialogContent className='sm:max-w-md'>
          <form
            onSubmit={(event) => {
              event.preventDefault()
              submitRename()
            }}
          >
            <DialogHeader>
              <DialogTitle>{t('navigator.renameTitle')}</DialogTitle>
              <DialogDescription>{t('navigator.renameDescription')}</DialogDescription>
            </DialogHeader>
            <Input
              autoFocus
              value={renameValue}
              aria-label={t('navigator.pageName')}
              className='mt-4'
              onChange={(event) => setRenameValue(event.currentTarget.value)}
            />
            <DialogFooter className='mt-4'>
              <Button type='button' variant='ghost' onClick={closeRename}>
                {t('common.cancel')}
              </Button>
              <Button type='submit' disabled={!renameValue.trim()}>
                {t('navigator.rename')}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      <Dialog
        open={chapterDialog !== null}
        onOpenChange={(open) => {
          if (!open) setChapterDialog(null)
        }}
      >
        <DialogContent className='sm:max-w-md'>
          <form
            onSubmit={(event) => {
              event.preventDefault()
              submitChapter()
            }}
          >
            <DialogHeader>
              <DialogTitle>
                {chapterDialog?.mode === 'create'
                  ? t('chapter.new')
                  : chapterDialog?.mode === 'rename'
                    ? t('chapter.renameTitle')
                    : t('chapter.deleteTitle')}
              </DialogTitle>
              <DialogDescription>
                {chapterDialog?.mode === 'delete'
                  ? t('chapter.deleteDescription', {
                      title: chapterDialog.chapter.title,
                      count: chapterDialog.chapter.pages.length,
                    })
                  : t('chapter.titleDescription')}
              </DialogDescription>
            </DialogHeader>
            {chapterDialog?.mode !== 'delete' ? (
              <Input
                autoFocus
                value={chapterTitle}
                aria-label={t('chapter.name')}
                className='mt-4'
                onChange={(event) => setChapterTitle(event.currentTarget.value)}
              />
            ) : null}
            <DialogFooter className='mt-4'>
              <Button type='button' variant='ghost' onClick={() => setChapterDialog(null)}>
                {t('common.cancel')}
              </Button>
              <Button
                type='submit'
                variant={chapterDialog?.mode === 'delete' ? 'destructive' : 'default'}
                disabled={chapterDialog?.mode !== 'delete' && !chapterTitle.trim()}
              >
                {chapterDialog?.mode === 'delete' ? t('navigator.delete') : t('common.save')}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  )
}

function ChapterHeader({
  chapter,
  index,
  last,
  active,
  collapsed,
  onOpen,
  onToggle,
  onRename,
  onMove,
  onDelete,
  onImport,
  importing,
  busy,
  onDrop,
}: {
  chapter: ChapterInfo
  index: number
  last: boolean
  active: boolean
  collapsed: boolean
  onOpen: () => void
  onToggle: () => void
  onRename: () => void
  onMove: (offset: number) => void
  onDelete: () => void
  onImport: (source: PageImportSource) => void
  importing: boolean
  busy: boolean
  onDrop: () => void
}) {
  const { t } = useTranslation()
  return (
    <div
      className={cn(
        'group flex h-7 items-center gap-1 rounded-md pr-1 pl-0.5 select-none',
        active ? 'bg-primary/[0.07]' : 'hover:bg-foreground/[0.045]',
      )}
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => {
        event.preventDefault()
        onDrop()
      }}
    >
      <Button
        variant='ghost'
        size='icon-xs'
        className='size-5 shrink-0 text-muted-foreground'
        aria-label={collapsed ? t('chapter.expand') : t('chapter.collapse')}
        onClick={onToggle}
      >
        <ChevronRight className={cn('size-3 transition-transform', !collapsed && 'rotate-90')} />
      </Button>
      <button
        type='button'
        className='flex min-w-0 flex-1 items-center gap-1.5 text-left'
        onClick={onOpen}
      >
        <BookOpen className='size-3 shrink-0 text-muted-foreground' />
        <span className='truncate text-[10px] font-semibold'>{chapter.title}</span>
      </button>
      <span className='text-[9px] text-muted-foreground tabular-nums'>{chapter.pages.length}</span>
      <span
        className={cn(
          'size-1.5 rounded-full',
          chapter.has_setting ? 'bg-primary' : chapter.has_summary ? 'bg-amber-500' : 'bg-border',
        )}
        aria-hidden='true'
      />
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant='ghost'
              size='icon-xs'
              aria-label={t('chapter.actionsFor', { title: chapter.title })}
              className='size-5 shrink-0 opacity-0 group-hover:opacity-100 aria-expanded:opacity-100'
            />
          }
        >
          <MoreHorizontal />
        </DropdownMenuTrigger>
        <DropdownMenuContent align='end'>
          <DropdownMenuItem
            disabled={busy}
            onClick={() =>
              void call(commands.runChapters, [chapter.id], null, true, false).catch(
                () => undefined,
              )
            }
          >
            <Sparkles /> {t('chapter.run')}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={onRename}>{t('chapter.rename')}</DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem disabled={importing} onClick={() => onImport('files')}>
            <FilePlus2 /> {t('chapter.importFiles')}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={importing} onClick={() => onImport('folder')}>
            <FolderOpen /> {t('chapter.importFolder')}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem disabled={index === 0} onClick={() => onMove(-1)}>
            {t('chapter.moveUp')}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={last} onClick={() => onMove(1)}>
            {t('chapter.moveDown')}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant='destructive' onClick={onDelete}>
            <Trash2 /> {t('navigator.delete')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

function EmptyChapterRow({
  importing,
  onImport,
}: {
  importing: boolean
  onImport: (source: PageImportSource) => void
}) {
  const { t } = useTranslation()
  return (
    <div className='ml-4 flex h-7 items-center gap-1.5 rounded-md border border-dashed border-border/70 px-1.5'>
      <span className='truncate text-[9px] text-muted-foreground'>{t('chapter.emptyHint')}</span>
      <div className='ml-auto shrink-0'>
        <PageImportMenu importing={importing} onImport={onImport} compact />
      </div>
    </div>
  )
}

function PageImportMenu({
  importing,
  onImport,
  compact = false,
}: {
  importing: boolean
  onImport: (source: PageImportSource) => void
  compact?: boolean
}) {
  const { t } = useTranslation()
  const icon = importing ? (
    <LoaderCircle className='size-3.5 animate-spin' />
  ) : (
    <FilePlus2 className='size-3.5' />
  )

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            size={compact ? 'icon-xs' : 'sm'}
            variant='outline'
            disabled={importing}
            aria-busy={importing}
            aria-label={importing ? t('navigator.importingAction') : t('navigator.importAction')}
            className={compact ? 'size-5 rounded-md' : 'rounded-lg text-[10px]'}
          />
        }
      >
        {icon}
        {compact ? null : importing ? t('navigator.importingAction') : t('navigator.importAction')}
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align='start'
        className='w-auto min-w-20 border border-border/50 p-0.5 shadow-sm ring-0'
      >
        <DropdownMenuItem
          disabled={importing}
          className='min-h-7 gap-1 px-1.5 py-0.5 text-[11px] [&_svg:not([class*="size-"])]:size-3.5'
          onClick={() => onImport('files')}
        >
          <FilePlus2 />
          {t('navigator.importFiles')}
        </DropdownMenuItem>
        <DropdownMenuItem
          disabled={importing}
          className='min-h-7 gap-1 px-1.5 py-0.5 text-[11px] [&_svg:not([class*="size-"])]:size-3.5'
          onClick={() => onImport('folder')}
        >
          <FolderOpen />
          {t('navigator.importFolder')}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function PageItem({
  page,
  active,
  selected,
  dragged,
  nested,
  onIntent,
  onSelect,
  onDragStart,
  onDragEnd,
  onRename,
  onDelete,
  onDrop,
  tabIndex,
  buttonRef,
  onNavigate,
}: {
  page: PageSummary
  active: boolean
  selected: boolean
  dragged: boolean
  nested: boolean
  onIntent?: () => void
  onSelect: (additive: boolean, range: boolean) => void
  onDragStart: () => void
  onDragEnd: () => void
  onRename: () => void
  onDelete: () => void
  onDrop: () => void
  tabIndex: number
  buttonRef: (node: HTMLButtonElement | null) => void
  onNavigate: (direction: 'next' | 'previous' | 'first' | 'last') => void
}) {
  const { t } = useTranslation()

  return (
    <article
      draggable
      data-active={active}
      data-selected={selected}
      className={cn(
        'group relative grid cursor-default grid-cols-[48px_minmax(0,1fr)] gap-2.5 rounded-xl p-1.5 transition-colors select-none',
        nested && 'ml-3',
        active
          ? 'bg-primary/[0.09] hover:bg-primary/[0.09]'
          : selected
            ? 'bg-foreground/[0.06] hover:bg-foreground/[0.08]'
            : 'hover:bg-foreground/[0.045]',
        dragged && 'opacity-50',
      )}
      onPointerEnter={onIntent}
      onFocus={onIntent}
      onClick={(event) => {
        if ((event.target as HTMLElement).closest('button,[role="menuitem"]')) return
        onSelect(event.ctrlKey || event.metaKey, event.shiftKey)
      }}
      onDoubleClick={onRename}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => {
        event.preventDefault()
        onDrop()
      }}
    >
      <button
        type='button'
        ref={buttonRef}
        tabIndex={tabIndex}
        data-page-open={page.id}
        aria-label={t('navigator.openPage', { page: page.label })}
        aria-current={active ? 'page' : undefined}
        aria-pressed={selected}
        className='absolute inset-0 z-10 rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-ring'
        onClick={(event) => onSelect(event.ctrlKey || event.metaKey, event.shiftKey)}
        onKeyDown={(event) => {
          const directions: Record<string, Parameters<typeof onNavigate>[0] | undefined> = {
            ArrowDown: 'next',
            ArrowUp: 'previous',
            Home: 'first',
            End: 'last',
          }
          const direction = directions[event.key]
          if (direction) {
            event.preventDefault()
            onNavigate(direction)
          }
        }}
      />
      <div className='grid h-16 w-12 place-items-center overflow-hidden rounded-lg bg-[var(--surface-well)]'>
        {page.source_asset ? (
          <PageThumbnail page={page.id} asset={page.source_asset} label={page.label} />
        ) : (
          <span className='text-[9px] text-muted-foreground'>{t('navigator.noImage')}</span>
        )}
      </div>
      <div className='min-w-0 py-0.5'>
        <div className='flex items-start gap-1'>
          <span className='min-w-0 flex-1 truncate text-[10px] font-medium'>{page.label}</span>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <Button
                  variant='ghost'
                  size='icon-xs'
                  aria-label={t('navigator.actionsFor', { page: page.label })}
                  className='relative z-20 -mt-1 shrink-0 opacity-0 shadow-none group-focus-within:opacity-100 group-hover:opacity-100 aria-expanded:opacity-100'
                />
              }
            >
              <MoreHorizontal />
            </DropdownMenuTrigger>
            <DropdownMenuContent align='end'>
              <DropdownMenuItem onClick={onRename}>{t('navigator.rename')}</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant='destructive' onClick={onDelete}>
                <Trash2 /> {t('navigator.delete')}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        <p className='mt-1 text-[9px] leading-3.5 text-muted-foreground'>
          {t('navigator.layerCount', { count: page.layer_count })}
        </p>
        <p className='mt-1 text-[9px] text-muted-foreground tabular-nums'>
          {page.size.width} × {page.size.height}
        </p>
      </div>
    </article>
  )
}

function PageThumbnail({ page, asset, label }: { page: string; asset: string; label: string }) {
  const { t } = useTranslation()
  const [source, setSource] = useState<string | null>(null)
  const [settled, setSettled] = useState(false)

  useEffect(() => {
    setSettled(false)
    const timeout = window.setTimeout(() => setSettled(true), 100)
    return () => window.clearTimeout(timeout)
  }, [asset])

  const thumbnail = useQuery({
    queryKey: ['thumbnail', asset],
    queryFn: async () => new Uint8Array(await call(commands.getThumbnail, page)),
    enabled: settled,
    staleTime: Number.POSITIVE_INFINITY,
    notifyOnChangeProps: ['data'],
  }).data

  useEffect(() => {
    if (!thumbnail) return
    const url = URL.createObjectURL(new Blob([thumbnail.buffer], { type: 'image/webp' }))
    setSource(url)
    return () => {
      URL.revokeObjectURL(url)
    }
  }, [thumbnail])

  return (
    <div className='grid size-full place-items-center'>
      {source ? (
        // eslint-disable-next-line @next/next/no-img-element
        <img src={source} alt={label} draggable={false} className='size-full object-contain' />
      ) : (
        <span className='text-[9px] text-muted-foreground'>{t('common.loading')}</span>
      )}
    </div>
  )
}
