'use client'

import { BookOpen, FolderInput, Pencil, Play, Plus, Sparkles, Square, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

import { call } from '@/lib/backend'
import {
  chapterKey,
  chaptersKey,
  pagesKey,
  projectKey,
  refresh,
  useChapter,
  useChapters,
  usePage,
} from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@koharu/ui/components/dialog'
import { Input } from '@koharu/ui/components/input'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Switch } from '@koharu/ui/components/switch'
import { Textarea } from '@koharu/ui/components/textarea'
import { cn } from '@koharu/ui/lib/utils'

const WIKI_SLUG = /^[a-z0-9_]{1,64}$/

const markdownClass =
  'min-w-0 text-[11px] leading-5 break-words text-muted-foreground [&_a]:text-primary [&_a]:underline [&_blockquote]:my-2 [&_blockquote]:border-l-2 [&_blockquote]:pl-3 [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_h1]:mt-3 [&_h1]:mb-1.5 [&_h1]:text-sm [&_h1]:font-semibold [&_h1]:text-foreground [&_h2]:mt-3 [&_h2]:mb-1 [&_h2]:text-xs [&_h2]:font-semibold [&_h2]:text-foreground [&_h3]:mt-2 [&_h3]:font-semibold [&_h3]:text-foreground [&_hr]:my-2 [&_li]:my-0.5 [&_ol]:my-1.5 [&_ol]:list-decimal [&_ol]:pl-4 [&_p]:my-1.5 [&_pre]:my-2 [&_pre]:overflow-x-auto [&_pre]:rounded [&_pre]:bg-muted [&_pre]:p-2 [&_table]:my-2 [&_table]:w-full [&_td]:border [&_td]:border-border [&_td]:p-1 [&_th]:border [&_th]:border-border [&_th]:p-1 [&_th]:text-left [&_ul]:my-1.5 [&_ul]:list-disc [&_ul]:pl-4 [&>*:first-child]:mt-0'

type Tab = 'summary' | 'wiki' | 'prompt'

export function ChapterPanel() {
  const { t } = useTranslation()
  const page = usePage().data
  const chapterId = page?.chapter ?? undefined
  const chapter = useChapter(chapterId)
  const chapters = useChapters().data ?? []
  const jobs = useKoharuStore((state) => state.jobs)
  const [tab, setTab] = useState<Tab>('summary')
  const [wikiFile, setWikiFile] = useState('characters')
  const [draft, setDraft] = useState<string | null>(null)
  const [editing, setEditing] = useState(false)
  const [pending, setPending] = useState(false)
  const [runOpen, setRunOpen] = useState(false)
  const [wikiOpen, setWikiOpen] = useState(false)
  const [wikiSlug, setWikiSlug] = useState('')

  const data = chapter.data
  useEffect(() => {
    setEditing(false)
    setDraft(null)
  }, [chapterId])

  // The backend runs one job at a time across the whole project, so every
  // chapter's controls have to yield to a job started from another chapter.
  const running = Object.values(jobs).find((job) => job.state === 'running')
  const busy = Boolean(running)
  const ownRunning = running?.chapter === chapterId ? running : undefined
  if (!data || !chapterId) return null

  const position = chapters.findIndex((value) => value.id === chapterId)
  const remaining = chapters.slice(Math.max(position, 0))
  // A chapter run owns a contiguous slice of the roster. `completed` is the
  // current chapter's index inside that slice, so the slice start is derivable.
  const start = running
    ? chapters.findIndex((value) => value.id === running.chapter) - running.completed
    : 0
  const queued = running !== undefined && position >= start && position < start + running.total
  // The summarizer rewrites this chapter's summary, setting, and wiki when it
  // reaches the chapter, so a save would be overwritten without warning.
  const summarizing = queued && (ownRunning ? ownRunning.phase === 'summarize' : data.summarize)
  const files = data.wiki.files
  const current = files.find((file) => file.slug === wikiFile) ?? files[0]
  const source =
    tab === 'summary'
      ? (data.summary ?? '')
      : tab === 'prompt'
        ? (data.setting ?? '')
        : (current?.body ?? '')
  const value = draft ?? source

  const invalidate = () =>
    refresh(chapterKey(chapterId), chaptersKey, projectKey, pagesKey).catch(() => undefined)

  const save = () => {
    if (draft === null) return
    const text = draft.trim() ? draft : null
    const command =
      tab === 'summary'
        ? call(commands.setChapterSummary, chapterId, text)
        : tab === 'prompt'
          ? call(commands.setChapterSetting, chapterId, text)
          : call(commands.setWiki, wikiFile, draft)
    setPending(true)
    void command
      .then(() => {
        setDraft(null)
        setEditing(false)
        return invalidate()
      })
      .catch(() => undefined)
      .finally(() => setPending(false))
  }

  const run = (summarize: boolean, translate: boolean, fromHere = false) => {
    setPending(true)
    const targets = fromHere ? remaining.map((value) => value.id) : [chapterId]
    void call(
      commands.runChapters,
      targets,
      summarize ? (fromHere ? null : true) : false,
      translate,
    )
      .catch(() => undefined)
      .finally(() => {
        setPending(false)
        setRunOpen(false)
      })
  }

  return (
    <div className='flex h-full min-h-0 flex-col'>
      <div className='border-b px-3 py-2'>
        <div className='flex items-center gap-1.5'>
          <BookOpen className='size-3.5 shrink-0 text-muted-foreground' />
          <h2 className='min-w-0 flex-1 truncate text-[11px] font-semibold'>{data.title}</h2>
          <span className='text-[9px] text-muted-foreground tabular-nums'>
            {t('chapter.pageCount', { count: data.pages.length })}
          </span>
        </div>
        <label className='mt-2 flex items-center gap-2 text-[10px] text-muted-foreground'>
          <Switch
            size='sm'
            checked={data.summarize}
            onCheckedChange={(checked) => {
              void call(commands.setChapterSummarize, chapterId, checked)
                .then(() => refresh(chaptersKey, chapterKey(chapterId)))
                .catch(() => undefined)
            }}
          />
          {t('chapter.summarizeToggle')}
        </label>
        <div className='mt-2 grid grid-cols-2 gap-1'>
          <Button
            size='sm'
            variant='outline'
            className='h-7 text-[10px]'
            disabled={pending || busy}
            onClick={() => {
              setPending(true)
              void call(commands.summarizeChapter, chapterId)
                .catch(() => undefined)
                .finally(() => setPending(false))
            }}
          >
            <Sparkles className='size-3' /> {t('chapter.summarize')}
          </Button>
          <Button
            size='sm'
            className='h-7 text-[10px]'
            disabled={pending || busy}
            onClick={() => run(false, true)}
          >
            <Play className='size-3' /> {t('chapter.translate')}
          </Button>
          <Button
            size='sm'
            variant='secondary'
            className='col-span-2 h-7 text-[10px]'
            disabled={pending || busy}
            onClick={() => run(true, true)}
          >
            <Sparkles className='size-3' /> {t('chapter.summarizeAndTranslate')}
          </Button>
          <Button
            size='sm'
            variant='ghost'
            className='col-span-2 h-6 text-[10px] text-muted-foreground'
            disabled={pending || busy}
            onClick={() => setRunOpen(true)}
          >
            {t('chapter.runFromHere', { count: remaining.length })}
          </Button>
        </div>
        {ownRunning ? (
          <div className='mt-2 rounded-md bg-primary/[0.07] p-2'>
            <div className='flex items-center gap-1.5 text-[10px] font-medium'>
              <span className='size-1.5 rounded-full bg-primary' />
              {ownRunning.phase === 'summarize'
                ? t('chapter.summarizing')
                : t('chapter.translating')}
              <Button
                size='icon-xs'
                variant='ghost'
                className='ml-auto'
                aria-label={t('activity.stop')}
                onClick={() => void call(commands.stopJob, ownRunning.id).catch(() => undefined)}
              >
                <Square className='size-2.5 fill-current' />
              </Button>
            </div>
            {ownRunning.detail ? (
              <p className='mt-1 line-clamp-2 text-[9px] leading-3.5 text-muted-foreground'>
                {ownRunning.detail}
              </p>
            ) : null}
          </div>
        ) : running ? (
          <p className='mt-2 rounded-md bg-muted/60 p-2 text-[10px] leading-4 text-muted-foreground'>
            {t('chapter.otherRunning', {
              title: running.chapter_title ?? t('chapter.title'),
            })}
          </p>
        ) : null}
      </div>

      <div className='flex shrink-0 gap-0.5 border-b px-2 py-1'>
        {(['summary', 'wiki', 'prompt'] as const).map((item) => (
          <Button
            key={item}
            size='sm'
            variant={tab === item ? 'secondary' : 'ghost'}
            className='h-6 flex-1 px-1 text-[10px]'
            onClick={() => {
              setTab(item)
              setDraft(null)
              setEditing(false)
            }}
          >
            {t(`chapter.tab_${item}`)}
          </Button>
        ))}
        <Button
          size='icon-xs'
          variant='ghost'
          className='h-6'
          aria-label={t('chapter.edit')}
          disabled={summarizing}
          onClick={() => {
            setDraft(source)
            setEditing((current) => !current)
          }}
        >
          <Pencil className='size-3' />
        </Button>
      </div>

      {tab === 'wiki' ? (
        <div className='flex shrink-0 flex-wrap items-center gap-0.5 border-b px-2 py-1'>
          {files.map((file) => (
            <Button
              key={file.slug}
              size='sm'
              variant={current?.slug === file.slug ? 'secondary' : 'ghost'}
              className='h-6 px-1.5 text-[9px]'
              onClick={() => {
                setWikiFile(file.slug)
                setDraft(null)
                setEditing(false)
              }}
            >
              {t(`chapter.wiki_${file.slug}`, { defaultValue: file.slug })}
            </Button>
          ))}
          <Button
            size='icon-xs'
            variant='ghost'
            className='h-6'
            aria-label={t('chapter.wikiImport')}
            onClick={() => {
              void call(commands.importWiki)
                .then(invalidate)
                .catch(() => undefined)
            }}
          >
            <FolderInput className='size-3' />
          </Button>
          <Button
            size='icon-xs'
            variant='ghost'
            className='h-6'
            aria-label={t('chapter.wikiAdd')}
            onClick={() => {
              setWikiSlug('')
              setWikiOpen(true)
            }}
          >
            <Plus className='size-3' />
          </Button>
          {current && files.length > 1 ? (
            <Button
              size='icon-xs'
              variant='ghost'
              className='h-6'
              aria-label={t('chapter.wikiRemove')}
              onClick={() => {
                void call(commands.deleteWikiFile, current.slug)
                  .then(invalidate)
                  .catch(() => undefined)
              }}
            >
              <Trash2 className='size-3' />
            </Button>
          ) : null}
        </div>
      ) : null}

      <ScrollArea className='min-h-0 flex-1' viewportClassName='p-3'>
        {summarizing && !ownRunning ? (
          <p className='mb-2 rounded-md bg-muted/60 p-2 text-[10px] leading-4 text-muted-foreground'>
            {t('chapter.summarizeLocked')}
          </p>
        ) : null}
        {editing ? (
          <div className='flex h-full min-h-0 flex-col gap-2'>
            <Textarea
              value={value}
              className='min-h-72 flex-1 resize-none font-mono text-[10px] leading-4'
              onChange={(event) => setDraft(event.currentTarget.value)}
            />
            <div className='flex gap-1'>
              <Button
                size='sm'
                className='h-7 text-[10px]'
                disabled={pending || summarizing}
                onClick={save}
              >
                {t('common.save')}
              </Button>
              <Button
                size='sm'
                variant='ghost'
                className='h-7 text-[10px]'
                onClick={() => {
                  setDraft(null)
                  setEditing(false)
                }}
              >
                {t('common.cancel')}
              </Button>
            </div>
          </div>
        ) : source.trim() ? (
          <div className={cn(markdownClass, tab !== 'summary' && 'font-mono text-[10px]')}>
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{source}</ReactMarkdown>
          </div>
        ) : (
          <p className='text-[10px] leading-4 text-muted-foreground'>{t(`chapter.empty_${tab}`)}</p>
        )}
      </ScrollArea>

      <Dialog open={runOpen} onOpenChange={setRunOpen}>
        <DialogContent className='sm:max-w-md'>
          <DialogHeader>
            <DialogTitle>{t('chapter.runFromHereTitle')}</DialogTitle>
            <DialogDescription>
              {t('chapter.runFromHereDescription', { count: remaining.length })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant='ghost' onClick={() => setRunOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button disabled={busy} onClick={() => run(true, true, true)}>
              {t('chapter.start')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={wikiOpen} onOpenChange={setWikiOpen}>
        <DialogContent className='sm:max-w-sm'>
          <DialogHeader>
            <DialogTitle>{t('chapter.wikiAddTitle')}</DialogTitle>
            <DialogDescription>{t('chapter.wikiAddDescription')}</DialogDescription>
          </DialogHeader>
          <Input
            value={wikiSlug}
            placeholder={t('chapter.wikiAddPlaceholder')}
            className='h-8 text-[11px]'
            onChange={(event) => setWikiSlug(event.currentTarget.value)}
          />
          <DialogFooter>
            <Button variant='ghost' onClick={() => setWikiOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button
              disabled={!WIKI_SLUG.test(wikiSlug)}
              onClick={() => {
                void call(commands.addWikiFile, wikiSlug)
                  .then(() => {
                    setWikiFile(wikiSlug)
                    setWikiOpen(false)
                    return invalidate()
                  })
                  .catch(() => undefined)
              }}
            >
              {t('chapter.wikiAddAction')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
