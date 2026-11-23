'use client'

import { useMutation, useQuery } from '@tanstack/react-query'
import { FolderInput, Plus, Trash2 } from 'lucide-react'
import { useMemo, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'

import { CommitTextarea } from '@/components/controls/CommitTextarea'
import { Markdown } from '@/components/controls/Markdown'
import { PiModelHint } from '@/components/controls/PiModelHint'
import { queryClient, send, useChapterJobs, useProject, useProjectMetadata } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import {
  commands,
  type ChapterEdit,
  type ChapterMode,
  type ProjectId,
  type SummarizerConfig,
  type WikiFile,
} from '@koharu/ipc/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@koharu/ui/components/dialog'
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
              <Button size='sm' disabled={active} onClick={() => run('translate')}>
                {t('chapter.translate')}
              </Button>
            </div>
            <Button
              size='sm'
              variant='secondary'
              disabled={active}
              onClick={() => run('summarize_translate')}
            >
              {t('chapter.summarizeAndTranslate')}
            </Button>
            <Button
              size='sm'
              variant='outline'
              disabled={active}
              onClick={() => run('resume', true)}
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
              <TabsContent value='wiki' keepMounted>
                <ChapterWiki
                  project={project.id}
                  files={wiki}
                  editing={editing}
                  onEdit={() => setEditing(true)}
                  locked={job?.phase === 'summarizing'}
                />
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
          {settingsOpen && (
            <SummarizerSettings
              applyDefault={(summarize) => edit({ action: 'set_summarize_all', summarize })}
            />
          )}
        </details>
      </div>
    </ScrollArea>
  )
}

function ChapterWiki({
  project,
  files,
  editing,
  onEdit,
  locked,
}: {
  project: ProjectId
  files: WikiFile[]
  editing: boolean
  onEdit: () => void
  locked: boolean
}) {
  const { t } = useTranslation()
  const [selected, setSelected] = useState('glossary')
  const [adding, setAdding] = useState(false)
  const [slug, setSlug] = useState('')
  const file = files.find((entry) => entry.slug === selected) ?? files[0]
  const add = useMutation({
    mutationFn: (name: string) => commands.editChapter({ action: 'add_wiki', slug: name }, project),
    onSuccess: (_, name) => {
      setSelected(name)
      setAdding(false)
      onEdit()
    },
  })
  const importWiki = useMutation({
    mutationFn: () => commands.importWiki(project),
    onSuccess: ([first]) => {
      if (first) setSelected(first)
    },
  })
  const disabled = locked || add.isPending || importWiki.isPending
  return (
    <div className='grid min-w-0 gap-2'>
      <div className='flex min-w-0 items-center gap-1'>
        {file && (
          <NativeSelect
            className='min-w-0 flex-1 [&>select]:w-full'
            value={file.slug}
            aria-label={t('chapter.wiki')}
            onChange={(event) => setSelected(event.currentTarget.value)}
          >
            {files.map((entry) => (
              <NativeSelectOption key={entry.slug} value={entry.slug}>
                {entry.slug}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
        <Button
          size='icon-xs'
          variant='ghost'
          disabled={disabled}
          aria-label={t('chapter.wikiImport')}
          title={t('chapter.wikiImport')}
          onClick={() => importWiki.mutate()}
        >
          <FolderInput />
        </Button>
        <Button
          size='icon-xs'
          variant='ghost'
          disabled={disabled}
          aria-label={t('chapter.wikiAdd')}
          title={t('chapter.wikiAdd')}
          onClick={() => {
            setSlug('')
            setAdding(true)
          }}
        >
          <Plus />
        </Button>
        {file && (
          <Button
            size='icon-xs'
            variant='ghost'
            disabled={disabled}
            aria-label={t('chapter.wikiRemove')}
            title={t('chapter.wikiRemove')}
            onClick={() =>
              send(commands.editChapter, { action: 'remove_wiki', slug: file.slug }, project)
            }
          >
            <Trash2 />
          </Button>
        )}
      </div>
      {file &&
        (editing && !locked ? (
          <CommitTextarea
            key={file.slug}
            aria-label={t('chapter.wiki')}
            value={file.body}
            onCommit={(body) =>
              send(commands.editChapter, { action: 'set_wiki', slug: file.slug, body }, project)
            }
          />
        ) : (
          <Markdown>{file.body}</Markdown>
        ))}
      <Dialog open={adding} onOpenChange={setAdding}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('chapter.wikiAddTitle')}</DialogTitle>
            <DialogDescription>{t('chapter.wikiAddDescription')}</DialogDescription>
          </DialogHeader>
          <form
            className='grid gap-3'
            onSubmit={(event) => {
              event.preventDefault()
              add.mutate(slug.trim())
            }}
          >
            <Input
              value={slug}
              aria-label={t('chapter.wikiAddTitle')}
              placeholder={t('chapter.wikiAddPlaceholder')}
              onChange={(event) => setSlug(event.currentTarget.value)}
            />
            <DialogFooter>
              <Button type='submit' disabled={disabled || !/^[a-z0-9_]{1,64}$/.test(slug.trim())}>
                {t('chapter.wikiAddAction')}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  )
}

const summarizerKey = ['summarizer'] as const

function SummarizerSettings({ applyDefault }: { applyDefault: (value: boolean) => void }) {
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
      {agent.backend === 'pi' && <PiModelHint />}
      <label className='grid gap-1'>
        {t('chapter.executable')}
        <Input
          value={agent.binary ?? ''}
          onChange={(event) => update({ binary: event.currentTarget.value || null })}
        />
      </label>
      <label className='grid gap-1'>
        {t('agent.reasoning')}
        <Input
          value={agent.effort ?? ''}
          onChange={(event) => update({ effort: event.currentTarget.value || null })}
        />
      </label>
      <label className='grid gap-1'>
        {t('chapter.summarizerSkill')}
        <Input
          value={agent.skill ?? ''}
          onChange={(event) => update({ skill: event.currentTarget.value || null })}
        />
      </label>
      <label className='flex items-center gap-2'>
        <Checkbox
          checked={agent.summarize_default ?? true}
          onCheckedChange={(checked) => update({ summarize_default: checked === true })}
        />
        {t('chapter.summarizerDefault')}
      </label>
      <Button
        size='sm'
        variant='outline'
        type='button'
        onClick={() => applyDefault(agent.summarize_default ?? true)}
      >
        {t('chapter.summarizerApplyDefault')}
      </Button>
      <Button size='sm' disabled={!draft} type='submit'>
        {t('chapter.saveSettings')}
      </Button>
    </form>
  )
}
