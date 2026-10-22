'use client'

import { Check, LoaderCircle, RefreshCw } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import {
  PreferencePage,
  PreferenceRow,
  PreferenceSection,
} from '@/components/preferences/PreferenceFields'
import { call } from '@/lib/backend'
import { queryClient, summarizerConfigKey, useSummarizerConfig } from '@/lib/queries'
import {
  commands,
  type SummarizerBackend,
  type SummarizerConfig,
  type SummarizerProbe,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Input } from '@koharu/ui/components/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@koharu/ui/components/select'

const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'] as const

const CUSTOM_MODEL = '__custom__'

/** `agy` rejects `--effort` when the model id already names one. */
function effortInModel(model: string | null): string | null {
  if (!model) return null
  return EFFORTS.find((effort) => model.endsWith(`-${effort}`)) ?? null
}

async function probeBackend(config: SummarizerConfig): Promise<SummarizerProbe> {
  try {
    return await call(commands.probeSummarizer, config)
  } catch (error) {
    return {
      path: null,
      version: null,
      models: [],
      error: error instanceof Error ? error.message : String(error),
    }
  }
}

export function SummarizerPreferences() {
  const { t } = useTranslation()
  const config = useSummarizerConfig().data
  const [draft, setDraft] = useState<SummarizerConfig | null>(config ?? null)
  const [probe, setProbe] = useState<SummarizerProbe | null>(null)
  const [probing, setProbing] = useState(false)
  const [customModel, setCustomModel] = useState(false)
  const [saved, setSaved] = useState(false)
  const savedTimer = useRef<number | undefined>(undefined)
  const queue = useRef(Promise.resolve())
  const probed = useRef(false)

  useEffect(() => {
    if (config) setDraft(config)
  }, [config])

  useEffect(
    () => () => {
      if (savedTimer.current) window.clearTimeout(savedTimer.current)
    },
    [],
  )

  const update = (value: Partial<SummarizerConfig>) => {
    setDraft((current) => (current ? { ...current, ...value } : current))
    setSaved(false)
  }

  useEffect(() => {
    if (!draft || !config) return
    if (JSON.stringify(draft) === JSON.stringify(config)) return
    const timeout = window.setTimeout(() => {
      queue.current = queue.current
        .catch(() => undefined)
        .then(async () => {
          const next = await call(commands.saveSummarizerConfig, draft)
          queryClient.setQueryData(summarizerConfigKey, next)
          if (savedTimer.current) window.clearTimeout(savedTimer.current)
          setSaved(true)
          savedTimer.current = window.setTimeout(() => setSaved(false), 1500)
        })
    }, 350)
    return () => window.clearTimeout(timeout)
  }, [config, draft])

  useEffect(() => {
    if (!draft || probed.current) return
    probed.current = true
    setProbing(true)
    void probeBackend(draft)
      .then(setProbe)
      .finally(() => setProbing(false))
  }, [draft])

  if (!draft) return null

  const models = probe?.models ?? []
  const listed = draft.model && models.includes(draft.model) ? [] : draft.model ? [draft.model] : []
  const modelItems = Object.fromEntries([
    ['', t('settings.summarizer.modelDefault')],
    ...models.map((model) => [model, model]),
    ...listed.map((model) => [model, model]),
    [CUSTOM_MODEL, t('settings.summarizer.modelCustom')],
  ])
  const modelEffort = effortInModel(draft.model ?? null)

  const runProbe = () => {
    setProbing(true)
    setProbe(null)
    void probeBackend(draft)
      .then(setProbe)
      .finally(() => setProbing(false))
  }

  return (
    <PreferencePage
      title={t('settings.summarizer.title')}
      description={t('settings.summarizer.description')}
    >
      <PreferenceSection title={t('settings.summarizer.backendSection')}>
        <PreferenceRow
          title={t('settings.summarizer.backend')}
          description={t('settings.summarizer.backendDescription')}
        >
          <Select
            value={draft.backend}
            items={{ agy: 'agy', pi: 'pi' }}
            onValueChange={(value) => {
              if (!value) return
              const next = { ...draft, backend: value as SummarizerBackend }
              update({ backend: next.backend })
              setProbe(null)
              setProbing(true)
              void probeBackend(next)
                .then(setProbe)
                .finally(() => setProbing(false))
            }}
          >
            <SelectTrigger className='h-8 w-40 text-[11px]'>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value='agy'>agy</SelectItem>
              <SelectItem value='pi'>pi</SelectItem>
            </SelectContent>
          </Select>
        </PreferenceRow>
        <PreferenceRow
          title={t('settings.summarizer.binary')}
          description={t('settings.summarizer.binaryDescription')}
        >
          <Input
            value={draft.binary ?? ''}
            placeholder={draft.backend === 'agy' ? 'agy' : 'pi'}
            className='h-8 w-72 text-[11px]'
            onChange={(event) => update({ binary: event.currentTarget.value || null })}
          />
        </PreferenceRow>
        <PreferenceRow
          title={t('settings.summarizer.model')}
          description={t('settings.summarizer.modelDescription')}
        >
          <div className='flex items-center gap-2'>
            <Select
              value={customModel ? CUSTOM_MODEL : (draft.model ?? '')}
              items={modelItems}
              onValueChange={(value) => {
                if (value === CUSTOM_MODEL) {
                  setCustomModel(true)
                  return
                }
                setCustomModel(false)
                update({ model: value || null })
              }}
            >
              <SelectTrigger className='h-8 w-72 text-[11px]'>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value=''>{t('settings.summarizer.modelDefault')}</SelectItem>
                {models.map((model) => (
                  <SelectItem key={model} value={model}>
                    {model}
                  </SelectItem>
                ))}
                <SelectItem value={CUSTOM_MODEL}>{t('settings.summarizer.modelCustom')}</SelectItem>
              </SelectContent>
            </Select>
            {customModel ? (
              <Input
                value={draft.model ?? ''}
                placeholder={t('settings.summarizer.modelPlaceholder')}
                className='h-8 w-56 text-[11px]'
                onChange={(event) => update({ model: event.currentTarget.value || null })}
              />
            ) : null}
          </div>
        </PreferenceRow>
        <PreferenceRow
          title={t('settings.summarizer.effort')}
          description={
            modelEffort
              ? t('settings.summarizer.effortInModel', { effort: modelEffort })
              : t('settings.summarizer.effortDescription')
          }
        >
          <Select
            value={draft.effort ?? ''}
            items={Object.fromEntries(
              ['', 'low', 'medium', 'high', 'xhigh', 'max'].map((value) => [
                value,
                value || t('settings.summarizer.effortDefault'),
              ]),
            )}
            disabled={modelEffort !== null}
            onValueChange={(value) => update({ effort: value || null })}
          >
            <SelectTrigger className='h-8 w-36 text-[11px]'>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {['', 'low', 'medium', 'high', 'xhigh', 'max'].map((value) => (
                <SelectItem key={value || 'default'} value={value}>
                  {value || t('settings.summarizer.effortDefault')}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </PreferenceRow>
        <PreferenceRow
          title={t('settings.summarizer.skill')}
          description={t('settings.summarizer.skillDescription')}
        >
          <Input
            value={draft.skill ?? ''}
            placeholder={t('settings.summarizer.skillPlaceholder')}
            className='h-8 w-72 text-[11px]'
            onChange={(event) => update({ skill: event.currentTarget.value || null })}
          />
        </PreferenceRow>
        <PreferenceRow
          title={t('settings.summarizer.summarizeDefault')}
          description={t('settings.summarizer.summarizeDefaultDescription')}
        >
          <Button
            variant={draft.summarize_default ? 'secondary' : 'outline'}
            size='sm'
            className='h-8 w-28 text-[10px]'
            onClick={() => update({ summarize_default: !draft.summarize_default })}
          >
            {draft.summarize_default
              ? t('settings.summarizer.enabled')
              : t('settings.summarizer.disabled')}
          </Button>
        </PreferenceRow>
      </PreferenceSection>

      <PreferenceSection title={t('settings.summarizer.probeSection')}>
        <div className='flex flex-wrap items-center gap-2 px-3 py-3'>
          <Button
            variant='outline'
            size='sm'
            className='h-8 text-[10px]'
            disabled={probing}
            onClick={runProbe}
          >
            {probing ? (
              <LoaderCircle className='size-3 animate-spin' />
            ) : (
              <RefreshCw className='size-3' />
            )}
            {t('settings.summarizer.probe')}
          </Button>
          {saved ? (
            <span className='flex items-center gap-1 text-[10px] text-muted-foreground'>
              <Check className='size-3' /> {t('common.saved')}
            </span>
          ) : null}
        </div>
        {probe ? (
          <div className='border-t px-3 py-3 text-[10px] leading-4 text-muted-foreground'>
            {probe.path ? <p className='break-all'>{probe.path}</p> : null}
            {probe.version ? <p>{probe.version}</p> : null}
            {probe.error ? <p className='text-destructive'>{probe.error}</p> : null}
            {probe.models.length > 0 ? (
              <p className='mt-2 line-clamp-6'>{probe.models.join(' · ')}</p>
            ) : null}
          </div>
        ) : null}
      </PreferenceSection>
    </PreferencePage>
  )
}
