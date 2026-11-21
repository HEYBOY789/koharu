'use client'

import { useQuery } from '@tanstack/react-query'
import { useMemo, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'

import { CommitTextarea } from '@/components/controls/CommitTextarea'
import { Markdown } from '@/components/controls/Markdown'
import { queryClient, send, useChapterJobs, useProject, useProjectMetadata } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import {
  commands,
  type ChapterEdit,
  type ChapterMode,
  type SummarizerConfig,
} from '@koharu/ipc/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@koharu/ui/components/tabs'

export function ChapterPanel() {
  const { t } = useTranslation()
  const project = useProject().data
  const selection = useKoharuStore((state) => state.selectedPages)
  const metadata = useProjectMetadata(project?.id).data
  const chapters = metadata?.chapters?.entries ?? []
  const chapter = chapters.find((entry) => project?.page && entry.pages.includes(project.page))
  const job = useChapterJobs().data?.find((entry) => entry.project === project?.id)
  const active = job?.phase === 'summarizing' || job?.phase === 'translating'
  const [title, setTitle] = useState('')
  const [wikiSlug, setWikiSlug] = useState('glossary')
  const [editing, setEditing] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const completed = useMemo(() => {
    const translated = new Set(
      project?.pages
        .filter((page) => page.texts > 0 && page.translated === page.texts)
        .map((page) => page.id),
    )
    return (metadata?.chapters?.entries ?? []).filter(
      (chapter) =>
        chapter.translated ||
        (chapter.pages.length > 0 && chapter.pages.every((id) => translated.has(id))),
    ).length
  }, [metadata, project?.pages])
  if (!project) return null
  const wiki = metadata?.wiki?.files ?? []
  const edit = (action: ChapterEdit) => send(commands.editChapter, action, project.id)
  const create = (event: FormEvent) => {
    event.preventDefault()
    if (!title.trim() || selection.length === 0) return
    void commands
      .editChapter({ action: 'create', title: title.trim(), pages: selection }, project.id)
      .then(() => setTitle(''))
      .catch(() => undefined)
  }
  const run = (mode: ChapterMode, fromHere = false) => {
    if (chapter) send(commands.runChapters, project.id, chapter.id, fromHere, mode)
  }

  return (
    <ScrollArea className='min-h-0 flex-1' viewportClassName='p-3'>
      <div className='grid gap-2'>
        {chapter ? (
          <>
            <div className='flex min-w-0 justify-between gap-2'>
              <h2 className='truncate text-sm font-semibold' title={chapter.title}>
                {chapter.title}
              </h2>
              <span className='shrink-0 text-xs text-muted-foreground tabular-nums'>
                {completed}/{chapters.length}
              </span>
            </div>
            <label className='flex items-center gap-2 text-xs'>
              <Checkbox
                checked={chapter.summarize}
                onCheckedChange={(checked) =>
                  edit({
                    action: 'set_summarize',
                    chapter: chapter.id,
                    summarize: checked === true,
                  })
                }
              />
              {t('chapter.summarizeBefore')}
            </label>
            <div className='grid grid-cols-2 gap-2'>
              <Button
                size='sm'
                variant='outline'
                disabled={active}
                onClick={() => run('summarize')}
              >
                {t('chapter.summarize')}
              </Button>
              <Button
                size='sm'
                disabled={active}
                onClick={() => run(chapter.summarize ? 'summarize_translate' : 'translate')}
              >
                {t('chapter.translate')}
              </Button>
            </div>
            <Button
              size='sm'
              variant='outline'
              disabled={active}
              onClick={() => run('summarize_translate', true)}
            >
              {t('chapter.runFromHere', { count: chapters.length - chapters.indexOf(chapter) })}
            </Button>
            <Tabs defaultValue='summary' className='min-w-0'>
              <TabsList variant='line'>
                <TabsTrigger value='summary'>{t('chapter.summary')}</TabsTrigger>
                <TabsTrigger value='wiki'>{t('chapter.wiki')}</TabsTrigger>
                <TabsTrigger value='prompt'>{t('chapter.prompt')}</TabsTrigger>
                <Button
                  size='sm'
                  variant='ghost'
                  onClick={() => setEditing((value) => !value)}
                  aria-pressed={editing}
                >
                  {t('chapter.edit')}
                </Button>
              </TabsList>
              <TabsContent value='summary'>
                {editing ? (
                  <CommitTextarea
                    key={chapter.id}
                    value={chapter.summary ?? ''}
                    onCommit={(summary) =>
                      edit({ action: 'set_summary', chapter: chapter.id, summary })
                    }
                  />
                ) : (
                  <Markdown>{chapter.summary || t('chapter.noSummary')}</Markdown>
                )}
              </TabsContent>
              <TabsContent value='wiki' className='grid gap-2'>
                {wiki.length > 0 && (
                  <NativeSelect
                    value={wikiSlug}
                    aria-label={t('chapter.wiki')}
                    onChange={(event) => setWikiSlug(event.currentTarget.value)}
                  >
                    {wiki.map((file) => (
                      <NativeSelectOption key={file.slug} value={file.slug}>
                        {file.slug}
                      </NativeSelectOption>
                    ))}
                  </NativeSelect>
                )}
                {editing ? (
                  <CommitTextarea
                    key={wikiSlug}
                    value={wiki.find((file) => file.slug === wikiSlug)?.body ?? ''}
                    onCommit={(body) => edit({ action: 'set_wiki', slug: wikiSlug, body })}
                  />
                ) : (
                  <Markdown>{wiki.find((file) => file.slug === wikiSlug)?.body ?? ''}</Markdown>
                )}
              </TabsContent>
              <TabsContent value='prompt'>
                {editing ? (
                  <CommitTextarea
                    key={chapter.id}
                    value={chapter.setting ?? ''}
                    onCommit={(prompt) =>
                      edit({ action: 'set_prompt', chapter: chapter.id, prompt })
                    }
                  />
                ) : (
                  <Markdown>{chapter.setting ?? ''}</Markdown>
                )}
              </TabsContent>
            </Tabs>
          </>
        ) : (
          <p className='text-xs text-muted-foreground'>{t('chapter.selectChapter')}</p>
        )}
        <details className='mt-2 border-t pt-2 text-xs'>
          <summary>{t('chapter.manage')}</summary>
          <form className='mt-2 grid gap-2' onSubmit={create}>
            <Input
              value={title}
              onChange={(event) => setTitle(event.currentTarget.value)}
              aria-label={t('chapter.name')}
              placeholder={t('chapter.name')}
            />
            <Button
              size='sm'
              variant='outline'
              type='submit'
              disabled={!title.trim() || selection.length === 0}
            >
              {t('chapter.createFromSelection')}
            </Button>
            {chapter && selection.length > 0 && (
              <Button
                size='sm'
                variant='ghost'
                type='button'
                onClick={() => edit({ action: 'assign', chapter: chapter.id, pages: selection })}
              >
                {t('chapter.assignSelection')}
              </Button>
            )}
          </form>
        </details>
        <details
          className='mt-2 border-t pt-2 text-xs'
          onToggle={(event) => setSettingsOpen(event.currentTarget.open)}
        >
          <summary>{t('chapter.summarizerSettings')}</summary>
          {settingsOpen && <SummarizerSettings />}
        </details>
      </div>
    </ScrollArea>
  )
}

const summarizerKey = ['summarizer'] as const

function SummarizerSettings() {
  const { t } = useTranslation()
  const result = useQuery({ queryKey: summarizerKey, queryFn: commands.getSummarizerSettings })
  const [draft, setDraft] = useState<SummarizerConfig | null>(null)
  const value = draft ?? result.data
  const agent = value
  const update = (patch: Partial<SummarizerConfig>) => setDraft({ ...agent, ...patch })
  if (!agent) return null
  return (
    <form
      className='mt-2 grid gap-2'
      onSubmit={(event) => {
        event.preventDefault()
        if (value)
          void commands
            .saveSummarizerSettings(value)
            .then(() => {
              queryClient.setQueryData(summarizerKey, value)
              setDraft(null)
            })
            .catch(() => undefined)
      }}
    >
      <label className='grid gap-1'>
        {t('chapter.backend')}
        <NativeSelect
          value={agent.backend ?? 'agy'}
          onChange={(event) =>
            update({ backend: event.currentTarget.value === 'pi' ? 'pi' : 'agy' })
          }
        >
          <NativeSelectOption value='agy'>AGY</NativeSelectOption>
          <NativeSelectOption value='pi'>Pi</NativeSelectOption>
        </NativeSelect>
      </label>
      <label className='grid gap-1'>
        {t('chapter.model')}
        <Input
          value={agent.model ?? ''}
          onChange={(event) => update({ model: event.currentTarget.value || null })}
        />
      </label>
      {agent.backend === 'pi' && (
        <p className='text-muted-foreground'>
          {t('chapter.piModelHint')}{' '}
          <code className='break-all'>local-proxy/ag/gemini-3.8-flash-high</code>
        </p>
      )}
      <label className='grid gap-1'>
        {t('chapter.executable')}
        <Input
          value={agent.binary ?? ''}
          onChange={(event) => update({ binary: event.currentTarget.value || null })}
        />
      </label>
      <Button size='sm' disabled={!draft} type='submit'>
        {t('chapter.saveSettings')}
      </Button>
    </form>
  )
}
