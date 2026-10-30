'use client'

import { Channel } from '@tauri-apps/api/core'
import { Bot, CircleStop, LogOut, RefreshCw, Send, Sparkles } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { useProject } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import {
  commands,
  type AgentStatus,
  type Backend,
  type CliConfig,
  type Config,
  type LoginEvent,
  type Reasoning,
} from '@koharu/bridge/protocol'
import { Badge } from '@koharu/ui/components/badge'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'
import { ScrollArea } from '@koharu/ui/components/scroll-area'
import { Textarea } from '@koharu/ui/components/textarea'

import { AgentProgress } from './AgentProgress'

export function AgentPanel() {
  const { t } = useTranslation()
  const [status, setStatus] = useState<AgentStatus | null>(null)
  const [error, setError] = useState('')
  const [checking, setChecking] = useState(false)
  const [saving, setSaving] = useState(false)
  const [pending, setPending] = useState(false)
  const [draft, setDraft] = useState<CliConfig>({
    binary: null,
    node: null,
    reasoning: null,
    model: null,
  })
  const [loginEvent, setLoginEvent] = useState<LoginEvent | null>(null)
  const [loggingIn, setLoggingIn] = useState(false)
  const [prompt, setPrompt] = useState('')
  const project = useProject().data
  const snapshot = useKoharuStore((state) => state.agent)
  const activeRun = snapshot.run?.state === 'running' ? snapshot.run : null
  const running = activeRun?.id ?? null
  const messages =
    snapshot.conversations.find((chat) => chat.project === project?.name)?.messages ?? []
  const ownRun = snapshot.run?.project === project?.name ? snapshot.run : null
  const alive = useRef(true)
  const checks = useRef(0)
  const requestPending = useRef(false)
  const submitted = useRef<string | null>(null)
  useEffect(() => {
    if (submitted.current && snapshot.run?.id === submitted.current) {
      setPending(false)
      requestPending.current = false
      submitted.current = null
    }
  }, [snapshot.sequence])

  useEffect(() => {
    alive.current = true
    void checkConnection()
    return () => {
      alive.current = false
      checks.current++
    }
  }, [])

  useEffect(() => {
    const backend = status?.config.backend
    if (status && backend && backend !== 'codex')
      setDraft(status.config[backend] ?? { binary: null, node: null, reasoning: null, model: null })
  }, [status?.config])

  const busy = running !== null || pending || saving || checking || loggingIn
  const backend = status?.config.backend ?? 'codex'
  const codex = {
    model: status?.config.codex?.model ?? null,
    reasoning: status?.config.codex?.reasoning ?? 'medium',
  }
  const cli = backend === 'codex' ? null : status?.config[backend]
  const model = backend === 'codex' ? codex.model : cli?.model
  const selectedModel = model
    ? status?.models.find((candidate) => candidate.id === model)
    : backend === 'codex'
      ? status?.models[0]
      : undefined

  if (!status)
    return (
      <div className='grid min-h-0 flex-1 content-center justify-items-center gap-3 p-4 text-xs'>
        <p
          role={error ? 'alert' : 'status'}
          className={error ? 'break-words text-destructive' : 'text-muted-foreground'}
        >
          {error || t('agent.loading')}
        </p>
        {error && (
          <Button size='sm' disabled={checking} onClick={() => void checkConnection()}>
            {t('agent.checkConnection')}
          </Button>
        )}
      </div>
    )

  return (
    <div className='flex min-h-0 flex-1 flex-col'>
      <div className='grid shrink-0 gap-2 border-b px-3 py-2.5'>
        <div className='flex items-end gap-2'>
          <label className='grid min-w-0 flex-1 gap-1 text-[10px]' htmlFor='editor-agent-backend'>
            {t('agent.backend')}
            <NativeSelect
              className='w-full min-w-0'
              size='sm'
              id='editor-agent-backend'
              value={backend}
              disabled={busy}
              onChange={(event) =>
                void saveConfig({ ...status.config, backend: event.target.value as Backend })
              }
            >
              <NativeSelectOption value='codex'>Codex</NativeSelectOption>
              <NativeSelectOption value='pi'>Pi</NativeSelectOption>
              <NativeSelectOption value='agy'>AGY</NativeSelectOption>
            </NativeSelect>
          </label>
          <Button
            variant='outline'
            size='icon-sm'
            aria-label={t('agent.checkConnection')}
            title={t('agent.checkConnection')}
            disabled={busy}
            onClick={() => void checkConnection()}
          >
            <RefreshCw className={checking ? 'animate-spin' : undefined} />
          </Button>
        </div>
        <p className='text-[10px] leading-4 text-muted-foreground'>{t('agent.backendHint')}</p>
        <Button
          variant='ghost'
          size='sm'
          disabled={busy}
          onClick={() => void call(commands.clearAgentSession)}
        >
          {t('agent.progress.newConversation')}
        </Button>
        {backend === 'codex' && status.account && (
          <div className='flex min-w-0 items-center gap-2'>
            <div className='min-w-0 flex-1'>
              <p className='truncate text-[11px] font-medium'>{status.account.email ?? 'Codex'}</p>
              <p className='truncate text-[9px] text-muted-foreground'>
                {status.account.plan ?? t('agent.account')}
              </p>
            </div>
            <Button
              variant='ghost'
              size='icon-sm'
              aria-label={t('agent.signOut')}
              disabled={busy}
              onClick={() => void logout()}
            >
              <LogOut />
            </Button>
          </div>
        )}
        {backend !== 'codex' && (
          <details className='text-[10px]'>
            <summary className='cursor-pointer py-1'>{t('agent.advancedSetup')}</summary>
            <form
              className='mt-2 grid gap-2'
              onSubmit={(event) => {
                event.preventDefault()
                void saveConfig({ ...status.config, [backend]: draft })
              }}
            >
              <label className='grid gap-1'>
                {t('agent.cliPath')}
                <Input
                  value={draft.binary ?? ''}
                  disabled={busy}
                  placeholder={t('agent.detectAutomatically')}
                  onChange={(event) =>
                    setDraft({ ...draft, binary: event.target.value.trim() || null })
                  }
                  className='text-xs'
                />
              </label>
              <label className='grid gap-1'>
                {t('agent.nodePath')}
                <Input
                  value={draft.node ?? ''}
                  disabled={busy}
                  placeholder={t('agent.detectAutomatically')}
                  onChange={(event) =>
                    setDraft({ ...draft, node: event.target.value.trim() || null })
                  }
                  className='text-xs'
                />
              </label>
              <label className='grid gap-1'>
                {t('agent.model')}
                <Input
                  value={draft.model ?? ''}
                  disabled={busy}
                  placeholder={t('agent.automatic')}
                  onChange={(event) =>
                    setDraft({ ...draft, model: event.target.value.trim() || null })
                  }
                  className='text-xs'
                />
              </label>
              <label className='grid gap-1'>
                {t('agent.reasoning')}
                <NativeSelect
                  className='w-full min-w-0'
                  size='sm'
                  value={draft.reasoning ?? ''}
                  disabled={busy}
                  onChange={(event) =>
                    setDraft({
                      ...draft,
                      reasoning: event.target.value ? (event.target.value as Reasoning) : null,
                    })
                  }
                >
                  <NativeSelectOption value=''>{t('agent.automatic')}</NativeSelectOption>
                  {(
                    [
                      'low',
                      'medium',
                      'high',
                      'xhigh',
                      ...(backend === 'agy' ? ['max'] : []),
                    ] as Reasoning[]
                  ).map((level) => (
                    <NativeSelectOption key={level} value={level}>
                      {t(`agent.reasoningLevels.${level}`)}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </label>
              <Button type='submit' variant='outline' size='sm' disabled={busy}>
                {t('agent.saveSetup')}
              </Button>
            </form>
          </details>
        )}
        {status.models.length > 0 && (
          <div
            className={backend === 'codex' ? 'grid grid-cols-[minmax(0,1fr)_7rem] gap-1.5' : 'grid'}
          >
            <NativeSelect
              className='w-full min-w-0'
              size='sm'
              value={model ?? ''}
              aria-label={t('agent.model')}
              disabled={busy}
              onChange={(event) => {
                const value = event.target.value || null
                if (backend === 'codex') {
                  const selected =
                    status.models.find((candidate) => candidate.id === value) ?? status.models[0]
                  const reasoning =
                    selected?.reasoning.length && !selected.reasoning.includes(codex.reasoning)
                      ? selected.reasoning[0]
                      : codex.reasoning
                  void saveConfig({ ...status.config, codex: { model: value, reasoning } })
                } else
                  void saveConfig({
                    ...status.config,
                    [backend]: { ...status.config[backend], model: value },
                  })
              }}
            >
              <NativeSelectOption value=''>{t('agent.automatic')}</NativeSelectOption>
              {model && !status.models.some((candidate) => candidate.id === model) && (
                <NativeSelectOption value={model}>{model}</NativeSelectOption>
              )}
              {status.models.map((model) => (
                <NativeSelectOption key={model.id} value={model.id}>
                  {model.name}
                </NativeSelectOption>
              ))}
            </NativeSelect>
            {backend === 'codex' && (
              <NativeSelect
                className='w-full min-w-0'
                size='sm'
                value={codex.reasoning}
                aria-label={t('agent.reasoning')}
                disabled={busy}
                onChange={(event) =>
                  void saveConfig({
                    ...status.config,
                    codex: { ...codex, reasoning: event.target.value as Reasoning },
                  })
                }
              >
                {(selectedModel?.reasoning.length
                  ? selectedModel.reasoning
                  : [codex.reasoning]
                ).map((reasoning) => (
                  <NativeSelectOption key={reasoning} value={reasoning}>
                    {t(`agent.reasoningLevels.${reasoning}`)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </div>
        )}
        {checking && (
          <p role='status' className='text-[10px] text-muted-foreground'>
            {t('agent.checking')}
          </p>
        )}
        {(error || status.error) && (
          <p role='alert' className='text-[11px] leading-4 break-words text-destructive'>
            {error || status.error}
          </p>
        )}
        {backend !== 'codex' && !status.ready && (
          <p className='text-[10px] leading-4 text-muted-foreground'>{t('agent.cliSignInHint')}</p>
        )}
      </div>
      {backend === 'codex' && !status.account ? (
        <div className='flex min-h-0 flex-1 flex-col items-center justify-center gap-3 p-5 text-center'>
          <Sparkles className='size-6 text-primary' />
          <h3 className='text-sm font-semibold'>{t('agent.signInTitle')}</h3>
          <p className='text-[11px] leading-5 text-muted-foreground'>
            {t('agent.signInDescription')}
          </p>
          {loginEvent?.type === 'device_code' && (
            <div className='w-full rounded-xl border bg-muted/40 p-3'>
              <p className='text-[10px]'>{t('agent.deviceCode')}</p>
              <p className='font-mono text-lg font-semibold tracking-widest select-text'>
                {loginEvent.user_code}
              </p>
              <p className='mt-1 text-[10px] text-muted-foreground'>{t('agent.deviceCodeHint')}</p>
            </div>
          )}
          <Button disabled={busy} onClick={() => void login()}>
            {loggingIn ? t('agent.signingIn') : t('agent.signIn')}
          </Button>
        </div>
      ) : (
        <>
          {activeRun && activeRun.project !== project?.name && (
            <div className='grid gap-2 border-b p-3 text-[11px]'>
              <p className='min-w-0 break-words'>
                {t('agent.progress.background', { name: activeRun.project })}
              </p>
              <AgentProgress run={activeRun} />
              <Button
                size='sm'
                variant='outline'
                className='w-full min-w-0'
                title={t('activity.openProject', { name: activeRun.project })}
                onClick={() => void call(commands.openProject, activeRun.project)}
              >
                <span className='min-w-0 truncate'>
                  {t('activity.openProject', { name: activeRun.project })}
                </span>
              </Button>
            </div>
          )}
          <ScrollArea className='min-h-0 flex-1'>
            {messages.length ? (
              <div className='grid gap-3 p-3'>
                {messages.map((message) => (
                  <div
                    key={message.id}
                    data-role={message.role}
                    className='flex data-[role=user]:justify-end'
                  >
                    <div
                      data-role={message.role}
                      data-error={message.error || undefined}
                      className='max-w-[90%] rounded-xl bg-muted px-3 py-2 text-[11px] leading-5 whitespace-pre-wrap select-text data-[error=true]:bg-destructive/10 data-[error=true]:text-destructive data-[role=user]:bg-primary data-[role=user]:text-primary-foreground'
                    >
                      {message.text ||
                        (ownRun?.id === message.id && ownRun.state === 'running' ? (
                          <AgentProgress run={ownRun} />
                        ) : message.role === 'assistant' ? (
                          t(message.state === 'stopped' ? 'agent.cancelled' : 'activity.finished')
                        ) : (
                          ''
                        ))}
                      {message.reasoning && (
                        <details className='mt-2 border-t pt-2 text-muted-foreground'>
                          <summary className='cursor-pointer'>
                            {t('agent.progress.summary')}
                          </summary>
                          <p className='mt-1 whitespace-pre-wrap'>{message.reasoning}</p>
                        </details>
                      )}
                    </div>
                  </div>
                ))}
                {ownRun && messages.at(-1)?.text && <AgentProgress run={ownRun} />}
              </div>
            ) : (
              <div className='grid min-h-64 place-items-center p-6 text-center'>
                <div>
                  <Bot className='mx-auto size-6 text-primary' />
                  <h3 className='mt-3 text-xs font-semibold'>{t('agent.emptyTitle')}</h3>
                  <p className='mt-1 max-w-56 text-[10px] leading-4 text-muted-foreground'>
                    {t('agent.emptyDescription')}
                  </p>
                </div>
              </div>
            )}
          </ScrollArea>

          <div className='shrink-0 border-t p-2.5'>
            <div className='rounded-xl border bg-background p-1.5 focus-within:border-ring'>
              <Textarea
                id='agent-message'
                value={prompt}
                disabled={busy || !status.ready}
                aria-label={t('agent.message')}
                placeholder={t('agent.placeholder')}
                className='max-h-36 min-h-16 resize-none border-0 bg-transparent px-1.5 py-1 text-[11px] leading-5 shadow-none focus-visible:ring-0'
                onChange={(event) => setPrompt(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' && !event.shiftKey) {
                    event.preventDefault()
                    void send()
                  }
                }}
              />
              <div className='flex items-center justify-between gap-2 pt-1'>
                <Badge
                  variant='outline'
                  className='h-5 max-w-44 truncate px-1.5 text-[9px] font-normal'
                >
                  {selectedModel?.name ?? model ?? t('agent.automatic')}
                </Badge>
                {running ? (
                  <Button
                    variant='outline'
                    size='icon-sm'
                    aria-label={t('activity.stop')}
                    onClick={() => void call(commands.cancelAgent, running).catch(() => undefined)}
                  >
                    <CircleStop />
                  </Button>
                ) : (
                  <Button
                    size='icon-sm'
                    aria-label={t('agent.send')}
                    disabled={busy || !status.ready || !prompt.trim()}
                    onClick={() => void send()}
                  >
                    <Send />
                  </Button>
                )}
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  )

  async function checkConnection() {
    const check = ++checks.current
    setChecking(true)
    setError('')
    try {
      const value = await call(commands.getAgentStatus)
      if (alive.current && check === checks.current) {
        setStatus(value)
      }
    } catch (cause) {
      if (alive.current && check === checks.current)
        setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (alive.current && check === checks.current) setChecking(false)
    }
  }

  async function saveConfig(config: Config) {
    if (busy) return
    setSaving(true)
    setError('')
    try {
      const saved = await call(commands.saveAgentConfig, config)
      if (!alive.current) return
      setStatus((current) => (current ? { ...current, config: saved } : current))
      await checkConnection()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (alive.current) setSaving(false)
    }
  }

  async function login() {
    setLoggingIn(true)
    setError('')
    const channel = new Channel<LoginEvent>()
    channel.onmessage = (event) => {
      if (alive.current) setLoginEvent(event)
    }
    try {
      setStatus(await call(commands.loginAgent, channel))
      setLoginEvent(null)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setLoggingIn(false)
    }
  }

  async function logout() {
    try {
      setStatus(await call(commands.logoutAgent))
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    }
  }

  async function send() {
    const message = prompt.trim()
    if (!message || requestPending.current || !status?.ready || !project || running) return
    requestPending.current = true
    setPending(true)
    setError('')
    try {
      const run = await call(commands.runAgent, message, project.name)
      submitted.current = run
      if (useKoharuStore.getState().agent.run?.id === run) {
        requestPending.current = false
        if (alive.current) setPending(false)
        submitted.current = null
      }
      if (alive.current) setPrompt('')
    } catch (cause) {
      requestPending.current = false
      if (alive.current) {
        setPending(false)
        setError(cause instanceof Error ? cause.message : String(cause))
      }
    }
  }
}
