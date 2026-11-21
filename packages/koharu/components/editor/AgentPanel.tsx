'use client'

import { useMutation } from '@tanstack/react-query'
import { Channel } from '@tauri-apps/api/core'
import { ArrowUp, Bot, Check, ChevronDown, ChevronRight, Square, SquarePen, X } from 'lucide-react'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import { Choice } from '@/components/controls/Choice'
import { Markdown } from '@/components/controls/Markdown'
import { ModelPicker } from '@/components/controls/ModelPicker'
import { PiModelHint } from '@/components/controls/PiModelHint'
import { modelName, REASONING } from '@/lib/models'
import {
  send,
  useModels,
  usePreferences,
  useProject,
  useProjectMetadata,
  useSavePreferences,
} from '@/lib/queries'
import {
  commands,
  type AgentBackend,
  type AgentConfig,
  type AgentEvent,
  type CliConfig,
  type Event,
} from '@koharu/ipc/protocol'
import { Button } from '@koharu/ui/components/button'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@koharu/ui/components/collapsible'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'
import { Popover, PopoverContent, PopoverTrigger } from '@koharu/ui/components/popover'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Spinner } from '@koharu/ui/components/spinner'
import { Textarea } from '@koharu/ui/components/textarea'
import { Tooltip, TooltipContent, TooltipTrigger } from '@koharu/ui/components/tooltip'
import { cn } from '@koharu/ui/lib/utils'

/** What a turn of the agent's showed, in the order it came. */
type Part =
  | { type: 'text'; text: string }
  /** Its reasoning, with when it began and, once text or a tool follows, ended. */
  | { type: 'thinking'; text: string; started: number; ended: number | null }
  | {
      type: 'tool'
      tool: string
      arguments: unknown
      state: 'running' | 'done' | 'failed'
      error: string | null
    }

type Turn =
  | { id: string; role: 'user'; text: string }
  | {
      id: string
      role: 'assistant'
      parts: Part[]
      state: 'running' | 'done' | 'failed' | 'cancelled'
      error?: string
    }

/** The in-app agent's conversation about the open project, as Claude Desktop and Codex lay a chat
 * out: the user's messages in bubbles, the agent's replies as plain text with its thinking and
 * tool calls folded beside them, and a composer holding the model, its reasoning, and New chat. */
export function AgentPanel() {
  const { t } = useTranslation()
  const [turns, setTurns] = useState<Turn[]>([])
  const [prompt, setPrompt] = useState('')
  const [running, setRunning] = useState<number | null>(null)
  const [choosing, setChoosing] = useState(false)
  const project = useProject().data
  const metadata = useProjectMetadata(project?.id).data
  const chapterPages = useMemo(() => {
    const labels = new Map<string, string>()
    for (const chapter of metadata?.chapters?.entries ?? []) {
      chapter.pages.forEach((page, index) => {
        const name = metadata?.page_labels?.[page]
        const position = `${chapter.title} · ${index + 1}/${chapter.pages.length}`
        labels.set(page, name ? `${position} · ${name}` : position)
      })
    }
    return labels
  }, [metadata])
  const busy = turns.some((turn) => turn.role === 'assistant' && turn.state === 'running')
  const models = (useModels().data ?? []).filter((model) => model.tools)
  const preferences = usePreferences().data
  const save = useSavePreferences()
  const list = useRef<HTMLDivElement>(null)
  // Whether the transcript follows what arrives: until the user scrolls up from its end.
  const following = useRef(true)
  const reset = useMutation({
    mutationFn: () => commands.resetAgent(),
    onSettled: () => {
      setTurns([])
      following.current = true
    },
  })

  useEffect(() => {
    const viewport = list.current
    if (viewport && following.current) viewport.scrollTop = viewport.scrollHeight
  }, [turns])

  if (!preferences) {
    return (
      <div className='grid min-h-0 flex-1 place-items-center p-6 text-[11px] text-muted-foreground'>
        {t('agent.loading')}
      </div>
    )
  }

  const config = preferences.agent
  const backend = config.backend ?? 'native'
  const cli = backend === 'pi' || backend === 'agy' ? backend : null
  const profile = cli ? config[cli] : undefined
  const configure = (patch: Partial<CliConfig>) => {
    if (cli) saveConfig({ ...config, [cli]: { ...profile, ...patch } })
  }
  const model = config.model ?? null
  const picker =
    'h-6 w-auto max-w-36 min-w-0 gap-1 border-0 bg-transparent px-1.5 text-[11px] text-muted-foreground shadow-none hover:bg-muted hover:text-foreground dark:bg-transparent dark:hover:bg-muted'

  return (
    <div className='flex min-h-0 flex-1 flex-col'>
      <div className='grid gap-2 border-b px-3 py-2 text-xs'>
        <label className='flex items-center gap-2'>
          <span className='shrink-0'>{t('agent.backend')}</span>
          <NativeSelect
            value={backend}
            disabled={busy}
            onChange={(event) => {
              saveConfig({ ...config, backend: event.currentTarget.value as AgentBackend })
              reset.mutate()
            }}
          >
            <NativeSelectOption value='native'>{t('agent.nativeBackend')}</NativeSelectOption>
            <NativeSelectOption value='pi'>Pi</NativeSelectOption>
            <NativeSelectOption value='agy'>AGY</NativeSelectOption>
            {backend === 'codex' && <NativeSelectOption value='codex'>Codex</NativeSelectOption>}
          </NativeSelect>
        </label>
        {cli && (
          <div className='grid gap-1' key={cli}>
            <label className='grid gap-1'>
              {t('agent.model')}
              <Input
                className='h-7'
                defaultValue={profile?.model ?? ''}
                placeholder={t('inference.providerDefault')}
                disabled={busy}
                onBlur={(event) => {
                  const model = event.currentTarget.value.trim() || null
                  if (model !== (profile?.model ?? null)) configure({ model })
                }}
              />
            </label>
            {cli === 'pi' && <PiModelHint />}
            <details>
              <summary>{t('publication.tools')}</summary>
              {(['binary', 'node', 'reasoning'] as const).map((field) => (
                <label className='mt-1 grid gap-1' key={field}>
                  {field === 'reasoning' ? t('agent.reasoning') : t(`publication.${field}`)}
                  <Input
                    className='h-7'
                    defaultValue={profile?.[field] ?? ''}
                    disabled={busy}
                    onBlur={(event) => {
                      const value = event.currentTarget.value.trim() || null
                      if (value !== (profile?.[field] ?? null)) configure({ [field]: value })
                    }}
                  />
                </label>
              ))}
            </details>
          </div>
        )}
      </div>
      <ScrollArea
        className='min-h-0 flex-1'
        viewportRef={list}
        viewportClassName='px-3 pt-3'
        onScrollCapture={() => {
          const viewport = list.current
          if (viewport) {
            following.current =
              viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 40
          }
        }}
      >
        {turns.length ? (
          <div className='grid gap-4 pb-3'>
            {turns.map((turn) =>
              turn.role === 'user' ? (
                <div
                  key={turn.id}
                  className='ml-auto max-w-[85%] rounded-2xl bg-muted px-3 py-2 text-xs leading-5 whitespace-pre-wrap select-text'
                >
                  {turn.text}
                </div>
              ) : (
                <AssistantTurn key={turn.id} turn={turn} pageName={pageName} />
              ),
            )}
          </div>
        ) : (
          <div className='grid min-h-72 place-items-center px-6 text-center'>
            <div>
              <Bot className='mx-auto size-6 text-primary' />
              <h3 className='mt-3 text-sm font-semibold'>{t('agent.emptyTitle')}</h3>
              <p className='mx-auto mt-1.5 max-w-60 text-[11px] leading-4 text-muted-foreground'>
                {t('agent.emptyDescription')}
              </p>
            </div>
          </div>
        )}
      </ScrollArea>

      <div className='shrink-0 p-2.5'>
        <div className='rounded-2xl border border-input bg-background p-2 shadow-xs transition-colors focus-within:border-ring'>
          <Textarea
            id='agent-message'
            value={prompt}
            aria-label={t('agent.message')}
            placeholder={t('agent.placeholder')}
            className='max-h-40 min-h-10 resize-none border-0 bg-transparent px-1 py-0.5 text-xs leading-5 shadow-none focus-visible:ring-0 dark:bg-transparent'
            onChange={(event) => setPrompt(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault()
                void submit()
              }
            }}
          />
          <div className='flex min-w-0 items-center gap-0.5 pt-1'>
            {!cli && (
              <Popover open={choosing} onOpenChange={setChoosing}>
                <PopoverTrigger
                  type='button'
                  aria-label={t('agent.model')}
                  className={cn(
                    'flex items-center rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring/40 data-open:bg-muted data-open:text-foreground',
                    picker,
                  )}
                >
                  <span className='truncate'>
                    {(model && modelName(models, model)) || t('inference.noModel')}
                  </span>
                  <ChevronDown className='size-3.5 shrink-0' />
                </PopoverTrigger>
                <PopoverContent
                  side='top'
                  align='start'
                  sideOffset={6}
                  className='w-64 gap-0 overflow-hidden p-1'
                >
                  <ModelPicker
                    value={model}
                    models={models}
                    onSelect={(next) => {
                      setChoosing(false)
                      saveConfig({ ...config, model: next })
                    }}
                  />
                </PopoverContent>
              </Popover>
            )}
            {!cli && (
              <Choice
                size='sm'
                side='top'
                value={config.reasoning ?? 'default'}
                choices={Object.fromEntries([
                  ['default', t('inference.providerDefault')],
                  ...REASONING.map((level) => [level, t(`reasoning.${level}`)]),
                ])}
                label={t('agent.reasoning')}
                className={picker}
                onChange={(reasoning) =>
                  saveConfig({ ...config, reasoning: reasoning === 'default' ? null : reasoning })
                }
              />
            )}
            <div className='ml-auto flex shrink-0 items-center gap-1'>
              <Tooltip>
                <TooltipTrigger
                  render={
                    <Button
                      type='button'
                      variant='ghost'
                      size='icon-sm'
                      aria-label={t('agent.newChat')}
                      disabled={busy || turns.length === 0 || reset.isPending}
                      className='size-7 rounded-full text-muted-foreground hover:text-foreground'
                      onClick={() => reset.mutate()}
                    />
                  }
                >
                  <SquarePen className='size-3.5' />
                </TooltipTrigger>
                <TooltipContent side='top'>{t('agent.newChat')}</TooltipContent>
              </Tooltip>
              {busy ? (
                <Button
                  type='button'
                  size='icon-sm'
                  aria-label={t('agent.cancel')}
                  className='size-7 rounded-full'
                  onClick={cancel}
                >
                  <Square className='size-3 fill-current' />
                </Button>
              ) : (
                <Button
                  type='button'
                  size='icon-sm'
                  aria-label={t('agent.send')}
                  disabled={!prompt.trim() || (!model && !cli)}
                  className='size-7 rounded-full'
                  onClick={() => void submit()}
                >
                  <ArrowUp className='size-4' />
                </Button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  )

  function saveConfig(agent: AgentConfig) {
    if (preferences) save.mutate({ ...preferences, agent })
  }

  function pageName(id: string): string | undefined {
    const chapter = chapterPages.get(id)
    if (chapter) return chapter
    const position = project?.pages.findIndex((page) => page.id === id) ?? -1
    return position < 0 ? undefined : t('navigator.pageNumber', { number: position + 1 })
  }

  function cancel() {
    if (running !== null) send(commands.stopAgent, running)
    // A cancelled run tells nothing more.
    setRunning(null)
    setTurns((current) =>
      current.map((turn) =>
        turn.role === 'assistant' && turn.state === 'running'
          ? { ...turn, parts: settle(turn.parts), state: 'cancelled' }
          : turn,
      ),
    )
  }

  async function submit() {
    const message = prompt.trim()
    if (!message || busy || (!model && !cli)) return
    const assistant = crypto.randomUUID()
    setPrompt('')
    following.current = true
    setTurns((current) => [
      ...current,
      { id: crypto.randomUUID(), role: 'user', text: message },
      { id: assistant, role: 'assistant', parts: [], state: 'running' },
    ])
    // A run can end before its id arrives.
    let settled = false

    const channel = new Channel<AgentEvent>()
    channel.onmessage = (update) => {
      if (update.type === 'event') {
        updateAssistant((turn) => ({ ...turn, parts: told(turn.parts, update.event) }))
        return
      }
      settled = true
      setRunning(null)
      updateAssistant((turn) =>
        update.type === 'completed'
          ? {
              ...turn,
              // A provider that doesn't stream tells its answer only at the end.
              parts: turn.parts.some((part) => part.type === 'text')
                ? settle(turn.parts)
                : [...settle(turn.parts), { type: 'text', text: update.answer }],
              state: 'done',
            }
          : { ...turn, parts: settle(turn.parts), state: 'failed', error: update.error },
      )
    }
    try {
      const run = await commands.runAgent(message, channel)
      if (!settled) setRunning(run)
    } catch (error) {
      updateAssistant((turn) => ({
        ...turn,
        parts: settle(turn.parts),
        state: 'failed',
        error: error instanceof Error ? error.message : t('agent.failed'),
      }))
      setRunning(null)
    }

    function updateAssistant(
      update: (turn: Extract<Turn, { role: 'assistant' }>) => Extract<Turn, { role: 'assistant' }>,
    ) {
      setTurns((current) =>
        current.map((turn) =>
          turn.id === assistant && turn.role === 'assistant' && turn.state === 'running'
            ? update(turn)
            : turn,
        ),
      )
    }
  }
}

/** `parts` with `event` told: text and reasoning run on in their part, and a tool's end settles
 * its call. Text or a tool ends the thinking before it. */
function told(parts: Part[], event: Event): Part[] {
  const last = parts.at(-1)
  switch (event.type) {
    case 'text':
      return last?.type === 'text'
        ? [...parts.slice(0, -1), { ...last, text: last.text + event.delta }]
        : [...settle(parts), { type: 'text', text: event.delta }]
    case 'reasoning':
      return last?.type === 'thinking' && last.ended === null
        ? [...parts.slice(0, -1), { ...last, text: last.text + event.delta }]
        : [...parts, { type: 'thinking', text: event.delta, started: Date.now(), ended: null }]
    case 'tool_started':
      return [
        ...settle(parts),
        {
          type: 'tool',
          tool: event.tool,
          arguments: event.arguments,
          state: 'running',
          error: null,
        },
      ]
    case 'tool_finished': {
      const at = parts.findLastIndex(
        (part) => part.type === 'tool' && part.tool === event.tool && part.state === 'running',
      )
      return at < 0
        ? parts
        : parts.map((part, index) =>
            index === at && part.type === 'tool'
              ? { ...part, state: event.error ? 'failed' : 'done', error: event.error }
              : part,
          )
    }
  }
}

/** `parts` with their thinking ended, as when the run moves on from it or ends. */
function settle(parts: Part[]): Part[] {
  return parts.map((part) =>
    part.type === 'thinking' && part.ended === null ? { ...part, ended: Date.now() } : part,
  )
}

/** The agent's turn: its parts in order, then how it ended when not with its answer. */
function AssistantTurn({
  turn,
  pageName,
}: {
  turn: Extract<Turn, { role: 'assistant' }>
  pageName: (id: string) => string | undefined
}) {
  const { t } = useTranslation()
  const last = turn.parts.at(-1)
  const target = turn.parts.findLast((part) => part.type === 'tool' && toolPage(part.arguments))
  const page = target?.type === 'tool' ? toolPage(target.arguments) : undefined
  const targetLabel = page ? pageName(page) : undefined
  // Something is on its way when nothing shows going on already.
  const waiting =
    turn.state === 'running' &&
    (last === undefined ||
      (last.type === 'tool' && last.state !== 'running') ||
      (last.type === 'thinking' && last.ended !== null))
  return (
    <div className='grid min-w-0 gap-2'>
      {turn.parts.map((part, index) =>
        part.type === 'text' ? (
          <Markdown key={index} className='text-foreground select-text'>
            {part.text}
          </Markdown>
        ) : part.type === 'thinking' ? (
          <Fold
            key={index}
            label={
              part.ended === null
                ? t('agent.thinking')
                : t('agent.thought', {
                    seconds: Math.max(1, Math.round((part.ended - part.started) / 1000)),
                  })
            }
            active={part.ended === null}
          >
            <p className='text-[11px] leading-4 whitespace-pre-wrap text-muted-foreground select-text'>
              {part.text}
            </p>
          </Fold>
        ) : (
          <Fold
            key={index}
            icon={
              part.state === 'running' ? (
                <Spinner className='size-3' />
              ) : part.state === 'done' ? (
                <Check className='size-3 text-emerald-600 dark:text-emerald-400' />
              ) : (
                <X className='size-3 text-destructive' />
              )
            }
            label={t(
              part.state === 'running'
                ? 'agent.using'
                : part.state === 'done'
                  ? 'agent.used'
                  : 'agent.toolFailed',
              { tool: part.tool },
            )}
          >
            <pre className='max-h-40 overflow-auto rounded-md bg-muted p-2 font-mono text-[10px] leading-4 whitespace-pre-wrap text-muted-foreground select-text'>
              {JSON.stringify(part.arguments, null, 2)}
            </pre>
            {part.error && (
              <p className='mt-1 text-[11px] leading-4 text-destructive select-text'>
                {part.error}
              </p>
            )}
          </Fold>
        ),
      )}
      {targetLabel && (
        <p className='text-[11px] leading-4 text-muted-foreground' title={targetLabel}>
          {t('agent.lastPage', { page: targetLabel })}
        </p>
      )}
      {waiting && (
        <p className='animate-pulse text-[11px] text-muted-foreground'>{t('agent.working')}</p>
      )}
      {turn.state === 'failed' && (
        <p className='text-xs leading-5 text-destructive select-text'>
          {turn.error || t('agent.failed')}
        </p>
      )}
      {turn.state === 'cancelled' && (
        <p className='text-[11px] text-muted-foreground'>{t('agent.cancelled')}</p>
      )}
    </div>
  )
}

function toolPage(value: unknown): string | undefined {
  if (!value || typeof value !== 'object') return undefined
  if ('page' in value && typeof value.page === 'string') return value.page
  if ('first_page' in value && typeof value.first_page === 'string') return value.first_page
  return undefined
}

/** A folded row of the agent's turn, as Claude folds its thinking and tool calls: its label, and
 * what it holds once opened. */
function Fold({
  icon,
  label,
  active = false,
  children,
}: {
  icon?: ReactNode
  label: string
  /** Still going, which its label shimmers to show. */
  active?: boolean
  children: ReactNode
}) {
  return (
    <Collapsible>
      <CollapsibleTrigger className='group flex max-w-full min-w-0 items-center gap-1.5 rounded-md py-0.5 text-[11px] text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/40'>
        {icon}
        <span className={active ? 'animate-pulse truncate' : 'truncate'}>{label}</span>
        <ChevronRight className='size-3 shrink-0 transition-transform group-data-[panel-open]:rotate-90' />
      </CollapsibleTrigger>
      <CollapsibleContent className='pt-1 pl-4'>{children}</CollapsibleContent>
    </Collapsible>
  )
}
