'use client'

import { useEffect, useMemo, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'

import { LocalAgentTools } from '@/components/app/LocalAgentTools'
import { call } from '@/lib/backend'
import { mangaDexLanguages } from '@/lib/mangadex'
import {
  commands,
  type PublicationConfig,
  type PublicationReadiness,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@koharu/ui/components/dialog'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@koharu/ui/components/select'

export function SourceImportDialog({
  open,
  onOpenChange,
  manga,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  manga?: { title: string; url: string; language: string; tags: string[] }
}) {
  const { t, i18n } = useTranslation()
  const [url, setURL] = useState(manga?.url ?? '')
  const [downloadURL, setDownloadURL] = useState('')
  const [language, setLanguage] = useState(manga?.language ?? 'en')
  const [name, setName] = useState(manga ? `${manga.title} [${manga.language}]` : '')
  const [summarize, setSummarize] = useState(false)
  const [translate, setTranslate] = useState(false)
  const [config, setConfig] = useState<PublicationConfig | null>(null)
  const [inspection, setInspection] = useState<{
    key: string
    status: PublicationReadiness
  }>()
  const [checking, setChecking] = useState(false)
  const [starting, setStarting] = useState(false)
  const [error, setError] = useState('')
  const [checkVersion, setCheckVersion] = useState(0)
  let catalog = false
  try {
    catalog = ['mangadex.org', 'www.mangadex.org'].includes(new URL(url).hostname)
  } catch {}
  const key = JSON.stringify(config ? { ...config, model: null, offline: false } : null)
  const checkConfig = useMemo<PublicationConfig | null>(() => JSON.parse(key), [key])
  const readiness = inspection?.key === key ? inspection.status : undefined
  let validURL = false
  try {
    const parsed = new URL(url.trim())
    validURL = ['http:', 'https:'].includes(parsed.protocol) && !parsed.username && !parsed.password
  } catch {
    /* A partial URL is expected while typing. */
  }
  useEffect(() => {
    if (!open) return
    let active = true
    setError('')
    setConfig(null)
    void call(commands.getSourceImportSettings)
      .then((value) => {
        if (active) setConfig(value)
      })
      .catch((cause) => {
        if (active) setError(String(cause))
      })
    return () => {
      active = false
    }
  }, [open])
  useEffect(() => {
    if (!open || !checkConfig) return
    let active = true
    setChecking(true)
    const timer = setTimeout(() => {
      void call(commands.inspectPublication, checkConfig, false)
        .then((status) => {
          if (active) setInspection({ key, status })
        })
        .catch((cause) => {
          if (active) setError(String(cause))
        })
        .finally(() => {
          if (active) setChecking(false)
        })
    }, 300)
    return () => {
      active = false
      clearTimeout(timer)
    }
  }, [open, checkConfig, key, checkVersion])
  const update = (value: Partial<PublicationConfig>) =>
    setConfig((current) => (current ? { ...current, ...value } : current))
  const locked = starting
  async function start(event: FormEvent) {
    event.preventDefault()
    if (!config || !readiness || readiness.missing.length || !validURL || locked) return
    setStarting(true)
    setError('')
    try {
      await call(
        commands.importSourceUrl,
        url.trim(),
        {
          ...config,
          node: readiness.node!,
          script: readiness.script!,
          binary: readiness.binary,
        },
        {
          language: catalog ? language : null,
          download_url: catalog ? downloadURL.trim() || null : null,
          project_name: name.trim() || null,
          summarize,
          translate,
        },
      )
      onOpenChange(false)
    } catch (cause) {
      setError(String(cause))
    } finally {
      setStarting(false)
    }
  }
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='max-h-[88vh] max-w-xl overflow-y-auto'>
        <DialogHeader>
          <DialogTitle>{t('sourceImport.title')}</DialogTitle>
          <DialogDescription>{t('sourceImport.description')}</DialogDescription>
        </DialogHeader>
        {error && (
          <p role='alert' className='text-sm break-words text-destructive'>
            {error}
          </p>
        )}
        {!config ? (
          <p>{t('common.loading')}</p>
        ) : (
          <form className='space-y-4' onSubmit={(event) => void start(event)}>
            <fieldset disabled={locked} className='space-y-3'>
              <label className='block space-y-1 text-xs'>
                {t('sourceImport.url')}
                <Input
                  required
                  type='url'
                  autoFocus
                  value={url}
                  readOnly={Boolean(manga)}
                  onChange={(event) => setURL(event.target.value)}
                />
              </label>
              {catalog && (
                <>
                  <p className='text-xs leading-5 text-muted-foreground'>
                    {t('sourceImport.sourceDiscovery')}
                  </p>
                  <label className='grid gap-2 text-xs'>
                    <span>{t('sourceImport.downloadLanguage')}</span>
                    <NativeSelect
                      value={language}
                      onChange={(event) => {
                        setLanguage(event.target.value)
                        if (manga) setName(`${manga.title} [${event.target.value}]`)
                      }}
                    >
                      {mangaDexLanguages.map((item) => (
                        <NativeSelectOption key={item.id} value={item.id}>
                          {new Intl.DisplayNames([i18n.language || 'en'], { type: 'language' }).of(
                            item.id === 'zh-hk'
                              ? 'zh-Hant'
                              : item.id === 'es-la'
                                ? 'es-419'
                                : item.id,
                          ) || item.fallback}
                        </NativeSelectOption>
                      ))}
                    </NativeSelect>
                  </label>
                  <div className='grid gap-2 text-xs'>
                    <label className='grid gap-2'>
                      <span>{t('sourceImport.downloadURL')}</span>
                      <Input
                        type='url'
                        value={downloadURL}
                        onChange={(event) => setDownloadURL(event.target.value)}
                        placeholder='https://…'
                        aria-describedby='source-download-hint'
                      />
                    </label>
                    <p id='source-download-hint' className='leading-5 text-muted-foreground'>
                      {t('sourceImport.downloadURLHint')}
                    </p>
                  </div>
                </>
              )}
              <label className='grid gap-2 text-xs'>
                <span>{t('sourceImport.projectName')}</span>
                <Input
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  placeholder={manga?.title || ''}
                />
              </label>
              {manga && (
                <p className='text-xs leading-5 text-muted-foreground'>
                  {t('mangadex.tags')}: {manga.tags.join(' · ')}
                </p>
              )}
              <label className='flex items-center gap-2 text-xs'>
                <Checkbox
                  checked={summarize}
                  onCheckedChange={(checked) => setSummarize(checked === true)}
                />
                {t('sourceImport.autoSummarize')}
              </label>
              <label className='flex items-center gap-2 text-xs'>
                <Checkbox
                  checked={translate}
                  onCheckedChange={(checked) => setTranslate(checked === true)}
                />
                {t('sourceImport.autoTranslate')}
              </label>
              <p className='text-xs leading-5 text-muted-foreground'>
                {t('sourceImport.autoHint')}
              </p>
              <div className='grid grid-cols-2 gap-3'>
                <label className='grid gap-2 text-xs'>
                  <span>{t('publication.agent')}</span>
                  <Select
                    value={config.backend}
                    onValueChange={(backend) => {
                      if (backend) update({ backend, binary: null, model: null })
                    }}
                  >
                    <SelectTrigger>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {['codex', 'pi', 'agy'].map((backend) => (
                        <SelectItem key={backend} value={backend}>
                          {backend}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </label>
                <label className='grid gap-2 text-xs'>
                  <span>{t('publication.model')}</span>
                  <Input
                    value={config.model ?? ''}
                    placeholder={t('settings.summarizer.modelPlaceholder')}
                    onChange={(event) => update({ model: event.target.value || null })}
                  />
                </label>
              </div>
              <p className='text-xs text-muted-foreground'>{t('publication.modelHint')}</p>
            </fieldset>
            <p role='status' className='text-xs'>
              {checking
                ? t('publication.checking')
                : readiness && !readiness.missing.length
                  ? t('publication.toolsReady')
                  : t('publication.toolsMissing', {
                      tools:
                        readiness?.missing.map((tool) => t(`publication.${tool}`)).join(', ') ||
                        t('publication.tools'),
                    })}
            </p>
            <LocalAgentTools
              config={config}
              readiness={readiness}
              disabled={locked}
              checking={checking}
              update={update}
              recheck={() => setCheckVersion((value) => value + 1)}
              onError={setError}
            />
            <p className='text-xs text-muted-foreground'>{t('sourceImport.progress')}</p>
            <Button
              type='submit'
              disabled={
                locked || checking || !validURL || !readiness || Boolean(readiness.missing.length)
              }
            >
              {starting ? t('common.loading') : t('sourceImport.start')}
            </Button>
          </form>
        )}
      </DialogContent>
    </Dialog>
  )
}
