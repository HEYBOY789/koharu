'use client'

import { useEffect, useMemo, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'

import { LocalAgentTools } from '@/components/app/LocalAgentTools'
import { call } from '@/lib/backend'
import { projectKey, refresh } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import {
  commands,
  type PublicationReadiness,
  type PublicationSettings,
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@koharu/ui/components/select'

const slugify = (text: string) =>
  text
    .normalize('NFD')
    .replace(/\p{M}/gu, '')
    .replace(/[đĐ]/g, 'd')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')

export function PublicationDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { t, i18n } = useTranslation()
  const [settings, setSettings] = useState<PublicationSettings | null>(null)
  const [starting, setStarting] = useState(false)
  const [genresText, setGenresText] = useState('')
  const [error, setError] = useState('')
  const [inspection, setInspection] = useState<{
    key: string
    status: PublicationReadiness
  }>()
  const [inspectionError, setInspectionError] = useState('')
  const [checking, setChecking] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [checkVersion, setCheckVersion] = useState(0)
  const busy = useKoharuStore((state) =>
    Object.values(state.jobs).some((job) => job.state === 'running'),
  )
  useEffect(() => {
    if (!open) return
    let active = true
    setError('')
    setSettings(null)
    void call(commands.getPublicationSettings)
      .then((value) => {
        if (active) {
          setGenresText(value.metadata.genres.map((genre) => genre.name).join(', '))
          setSettings({
            ...value,
            metadata: {
              ...value.metadata,
              slug: value.metadata.slug || slugify(value.metadata.title),
            },
          })
        }
      })
      .catch((cause) => {
        if (active) setError(String(cause))
      })
    return () => {
      active = false
    }
  }, [open])
  const metadata = settings?.metadata
  const config = settings?.config
  const configurationKey = JSON.stringify(
    config ? { ...config, model: null, offline: false } : null,
  )
  const inspectionConfig = useMemo<PublicationSettings['config'] | null>(
    () => JSON.parse(configurationKey),
    [configurationKey],
  )
  const readiness = inspection?.key === configurationKey ? inspection.status : undefined
  const catalog = readiness?.catalog
  const locked = busy || starting || refreshing
  useEffect(() => {
    if (!open || !inspectionConfig) return
    let active = true
    setChecking(true)
    setInspection(undefined)
    setInspectionError('')
    const timer = setTimeout(() => {
      void call(commands.inspectPublication, inspectionConfig, false)
        .then((status) => {
          if (active) setInspection({ key: configurationKey, status })
        })
        .catch((cause) => {
          if (active) setInspectionError(String(cause))
        })
        .finally(() => {
          if (active) setChecking(false)
        })
    }, 300)
    return () => {
      active = false
      clearTimeout(timer)
    }
  }, [open, configurationKey, inspectionConfig, checkVersion])
  useEffect(() => {
    if (readiness && !readiness.catalog && config?.offline) {
      setSettings((current) =>
        current ? { ...current, config: { ...current.config, offline: false } } : current,
      )
    }
  }, [readiness, config?.offline])
  const updateMetadata = (value: Partial<PublicationSettings['metadata']>) =>
    setSettings((current) =>
      current ? { ...current, metadata: { ...current.metadata, ...value } } : current,
    )
  const updateConfig = (value: Partial<PublicationSettings['config']>) =>
    setSettings((current) =>
      current ? { ...current, config: { ...current.config, ...value } } : current,
    )
  async function refreshCatalog() {
    if (!config) return
    setRefreshing(true)
    setInspectionError('')
    try {
      const status = await call(commands.inspectPublication, config, true)
      setInspection({ key: configurationKey, status })
    } catch (cause) {
      setInspectionError(String(cause))
    } finally {
      setRefreshing(false)
    }
  }
  async function start(event: FormEvent) {
    event.preventDefault()
    if (!settings || !readiness || readiness.missing.length || (config?.offline && !catalog)) return
    setStarting(true)
    setError('')
    try {
      const genres = genresText
        .split(',')
        .map((name) => name.trim())
        .filter(Boolean)
        .map((name) => ({ name, slug: slugify(name) }))
      const id = await call(
        commands.preparePublication,
        { ...settings.metadata, genres },
        {
          ...settings.config,
          node: readiness.node!,
          script: readiness.script!,
          binary: readiness.binary,
        },
      )
      await refresh(projectKey)
      if (id) onOpenChange(false)
    } catch (cause) {
      setError(String(cause))
    } finally {
      setStarting(false)
    }
  }
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='max-h-[88vh] max-w-2xl overflow-y-auto'>
        <DialogHeader>
          <DialogTitle>{t('publication.prepare')}</DialogTitle>
          <DialogDescription>{t('publication.description')}</DialogDescription>
        </DialogHeader>
        {error && (
          <p role='alert' className='text-sm text-destructive'>
            {error}
          </p>
        )}
        {!settings || !metadata || !config ? (
          <p>{t('common.loading')}</p>
        ) : (
          <form onSubmit={(event) => void start(event)} className='space-y-4'>
            <fieldset disabled={locked} className='grid grid-cols-2 gap-3'>
              <label className='space-y-1 text-xs'>
                {t('publication.story')}
                <Input
                  required
                  value={metadata.title}
                  onChange={(event) => updateMetadata({ title: event.target.value })}
                />
              </label>
              <label className='space-y-1 text-xs'>
                {t('publication.slug')}
                <Input
                  required
                  pattern='[a-z0-9]+(-[a-z0-9]+)*'
                  value={metadata.slug}
                  onChange={(event) => updateMetadata({ slug: event.target.value })}
                />
              </label>
              <label className='space-y-1 text-xs'>
                {t('about.author')}
                <Input
                  value={metadata.author}
                  onChange={(event) => updateMetadata({ author: event.target.value })}
                />
              </label>
              <label className='space-y-1 text-xs'>
                {t('publication.year')}
                <Input
                  type='number'
                  min={1000}
                  max={9999}
                  value={metadata.releaseYear ?? ''}
                  onChange={(event) =>
                    updateMetadata({
                      releaseYear: event.target.value ? Number(event.target.value) : null,
                    })
                  }
                />
              </label>
              <label className='space-y-1 text-xs'>
                {t('publication.country')}
                <Input
                  value={metadata.country}
                  onChange={(event) => updateMetadata({ country: event.target.value })}
                />
              </label>
              <label className='space-y-1 text-xs'>
                {t('publication.status')}
                <Select
                  value={metadata.status}
                  onValueChange={(value) => {
                    if (value) updateMetadata({ status: value })
                  }}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {['ongoing', 'completed', 'hiatus'].map((status) => (
                      <SelectItem key={status} value={status}>
                        {t(`publication.${status}`)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </label>
              <label className='space-y-1 text-xs'>
                {t('publication.genres')}
                <Input value={genresText} onChange={(event) => setGenresText(event.target.value)} />
              </label>
              <label className='col-span-2 space-y-1 text-xs'>
                {t('publication.aliases')}
                <Input
                  value={metadata.alternativeTitles}
                  onChange={(event) => updateMetadata({ alternativeTitles: event.target.value })}
                />
              </label>
            </fieldset>
            <fieldset disabled={locked} className='space-y-3'>
              <label className='block space-y-1 text-xs'>
                {t('publication.catalog')}
                <Input
                  type='url'
                  required
                  value={config.catalog_url}
                  onChange={(event) =>
                    updateConfig({
                      catalog_url: event.target.value,
                      offline: false,
                    })
                  }
                />
              </label>
              <div className='grid grid-cols-2 gap-3'>
                <label className='space-y-1 text-xs'>
                  {t('publication.agent')}
                  <Select
                    value={config.backend}
                    onValueChange={(value) => {
                      if (value)
                        updateConfig({
                          backend: value,
                          binary: null,
                          model: null,
                        })
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
                <label className='space-y-1 text-xs'>
                  {t('publication.model')}
                  <Input
                    value={config.model ?? ''}
                    placeholder={t('settings.summarizer.modelPlaceholder')}
                    onChange={(event) => updateConfig({ model: event.target.value || null })}
                  />
                </label>
              </div>
              <p className='text-xs text-muted-foreground'>{t('publication.modelHint')}</p>
            </fieldset>
            <section className='space-y-2 rounded-lg border p-3 text-xs'>
              <div role='status' aria-live='polite' className='space-y-1'>
                <p className='font-medium'>
                  {checking
                    ? t('publication.checking')
                    : catalog
                      ? t('publication.catalogSaved', {
                          count: catalog.storyCount,
                        })
                      : t('publication.catalogEmpty')}
                </p>
                {catalog && (
                  <>
                    <p className='break-all text-muted-foreground'>{catalog.siteUrl}</p>
                    <p className='text-muted-foreground'>
                      {t('publication.catalogDate', {
                        time: new Date(catalog.downloadedAt).toLocaleString(i18n.language),
                      })}
                    </p>
                  </>
                )}
              </div>
              {(inspectionError || readiness?.catalogError) && (
                <p role='alert' className='break-words text-destructive'>
                  {t('publication.checkFailed', {
                    error: inspectionError || readiness?.catalogError,
                  })}
                </p>
              )}
              <Button
                type='button'
                variant='outline'
                disabled={
                  locked || checking || !readiness?.node || !readiness.script || !config.catalog_url
                }
                onClick={() => void refreshCatalog()}
              >
                {refreshing ? t('publication.refreshing') : t('publication.refreshCatalog')}
              </Button>
              <label className='flex items-start gap-2 pt-1'>
                <Checkbox
                  checked={Boolean(config.offline && catalog)}
                  disabled={locked || checking || !catalog}
                  onCheckedChange={(checked) => updateConfig({ offline: checked })}
                />
                {t('publication.offline')}
              </label>
              <p className='text-muted-foreground'>
                {!catalog
                  ? t('publication.offlineUnavailable')
                  : config.offline
                    ? t('publication.offlineHint')
                    : t('publication.onlineHint')}
              </p>
            </section>
            {!checking && (
              <p role='status' className='text-xs'>
                {readiness && !readiness.missing.length
                  ? t('publication.toolsReady')
                  : t('publication.toolsMissing', {
                      tools:
                        readiness?.missing.map((tool) => t(`publication.${tool}`)).join(', ') ||
                        t('publication.tools'),
                    })}
              </p>
            )}
            <LocalAgentTools
              config={config}
              readiness={readiness}
              disabled={locked}
              checking={checking}
              update={updateConfig}
              recheck={() => setCheckVersion((value) => value + 1)}
              onError={setError}
            />
            <p className='text-xs text-muted-foreground'>{t('publication.progress')}</p>
            <Button
              type='submit'
              disabled={
                locked ||
                checking ||
                !readiness ||
                Boolean(readiness.missing.length) ||
                (config.offline && !catalog)
              }
            >
              {starting ? t('common.loading') : t('publication.prepare')}
            </Button>
          </form>
        )}
      </DialogContent>
    </Dialog>
  )
}
