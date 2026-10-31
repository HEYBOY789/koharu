'use client'

import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'

import { LocalAgentTools } from '@/components/app/LocalAgentTools'
import { call } from '@/lib/backend'
import { mangaDexLanguages } from '@/lib/mangadex'
import {
  commands,
  type PublicationConfig,
  type PublicationReadiness,
  type SourcePreview,
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

import { SourceChapterPicker } from './SourceChapterPicker'

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
  const [urlTouched, setURLTouched] = useState(false)
  const [readerTouched, setReaderTouched] = useState(false)
  const [previewing, setPreviewing] = useState(false)
  const [preview, setPreview] = useState<{ key: string; data: SourcePreview } | null>(null)
  const [chapters, setChapters] = useState<string[]>([])
  const previewRequest = useRef(0)
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
  let validReader = !downloadURL.trim()
  try {
    const address = new URL(downloadURL.trim())
    validReader =
      ['http:', 'https:'].includes(address.protocol) && !address.username && !address.password
  } catch {}
  const sourceKey = JSON.stringify([url.trim(), downloadURL.trim(), language, key])
  const survey = preview?.key === sourceKey ? preview.data : null
  useEffect(() => {
    if (!open) return
    let active = true
    setError('')
    setPreviewing(false)
    setPreview(null)
    setChapters([])
    setURLTouched(false)
    setReaderTouched(false)
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
      previewRequest.current++
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
  const locked = starting || previewing
  const canPreview =
    validURL &&
    (!catalog || validReader) &&
    readiness &&
    !readiness.missing.length &&
    !checking &&
    !locked
  async function inspectSource() {
    if (!config || !canPreview) return
    const request = ++previewRequest.current
    setPreviewing(true)
    setError('')
    try {
      const data = await call(commands.previewSourceUrl, url.trim(), config, {
        language: catalog ? language : null,
        download_url: catalog ? downloadURL.trim() || null : null,
        project_name: null,
        summarize: false,
        translate: false,
        chapters: null,
      })
      if (previewRequest.current === request) {
        if (data) {
          setPreview({ key: sourceKey, data })
          setChapters(data.chapters.map((chapter) => chapter.url))
        }
      }
    } catch (cause) {
      if (previewRequest.current === request) setError(String(cause))
    } finally {
      if (previewRequest.current === request) setPreviewing(false)
    }
  }
  async function start(event: FormEvent) {
    event.preventDefault()
    if (
      !config ||
      !readiness ||
      readiness.missing.length ||
      !validURL ||
      locked ||
      !survey ||
      !chapters.length
    )
      return
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
          download_url: catalog ? survey.source_url : null,
          project_name: name.trim() || null,
          summarize,
          translate,
          chapters,
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
      <DialogContent className='flex max-h-[90dvh] w-[calc(100vw-2rem)] flex-col overflow-hidden p-0 sm:max-w-3xl'>
        <DialogHeader className='shrink-0 px-6 pt-6'>
          <DialogTitle>{t('sourceImport.title')}</DialogTitle>
          <DialogDescription>{t('sourceImport.description')}</DialogDescription>
        </DialogHeader>
        {!config ? (
          <p
            role={error ? 'alert' : 'status'}
            className='overflow-y-auto px-6 pb-6 text-sm break-words'
          >
            {error || t('common.loading')}
          </p>
        ) : (
          <form className='flex min-h-0 flex-col' onSubmit={(event) => void start(event)}>
            <div className='min-h-0 flex-1 space-y-4 overflow-y-auto px-6 py-4'>
              {error && (
                <p role='alert' className='text-sm break-words text-destructive'>
                  {error}
                </p>
              )}
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
                    onBlur={() => setURLTouched(true)}
                    aria-invalid={(urlTouched && !validURL) || undefined}
                    aria-describedby={urlTouched && !validURL ? 'source-url-error' : undefined}
                  />
                </label>
                {urlTouched && !validURL && (
                  <p id='source-url-error' role='alert' className='text-xs text-destructive'>
                    {t('sourceImport.invalidURL')}
                  </p>
                )}
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
                            {new Intl.DisplayNames([i18n.language || 'en'], {
                              type: 'language',
                            }).of(
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
                          onBlur={() => setReaderTouched(true)}
                          aria-invalid={(readerTouched && !validReader) || undefined}
                          aria-describedby={
                            readerTouched && !validReader
                              ? 'source-reader-error'
                              : 'source-download-hint'
                          }
                        />
                      </label>
                      {readerTouched && !validReader && (
                        <p
                          id='source-reader-error'
                          role='alert'
                          className='text-xs text-destructive'
                        >
                          {t('sourceImport.invalidURL')}
                        </p>
                      )}
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
                {survey ? (
                  <>
                    <p className='text-xs font-medium'>{survey.title}</p>
                    <p className='text-xs break-all text-muted-foreground'>
                      {t('sourceImport.previewSource', { source: survey.source_url })}
                    </p>
                    <SourceChapterPicker
                      key={sourceKey}
                      chapters={survey.chapters}
                      selected={chapters}
                      onChange={setChapters}
                    />
                  </>
                ) : (
                  <p className='text-xs leading-5 text-muted-foreground'>
                    {t('sourceImport.previewHint')}
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
            </div>
            <div className='flex shrink-0 flex-wrap items-center justify-between gap-3 border-t bg-popover px-6 py-4'>
              <Button
                type='button'
                variant='outline'
                disabled={!canPreview}
                onClick={() => void inspectSource()}
              >
                {previewing ? t('sourceImport.previewing') : t('sourceImport.preview')}
              </Button>
              <Button
                type='submit'
                disabled={
                  locked ||
                  checking ||
                  !validURL ||
                  !readiness ||
                  Boolean(readiness.missing.length) ||
                  !survey ||
                  !chapters.length
                }
              >
                {starting ? t('common.loading') : t('sourceImport.start')}
              </Button>
            </div>
          </form>
        )}
      </DialogContent>
    </Dialog>
  )
}
