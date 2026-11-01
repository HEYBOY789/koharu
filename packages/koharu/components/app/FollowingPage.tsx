'use client'

import { openUrl } from '@tauri-apps/plugin-opener'
import {
  Bell,
  Check,
  ChevronLeft,
  ChevronRight,
  ExternalLink,
  FolderPlus,
  RefreshCw,
  Square,
  Trash2,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { MangaDexCover } from '@/components/app/MangaDexCover'
import { SourceImportDialog } from '@/components/app/SourceImportDialog'
import { mangaDexLanguages } from '@/lib/mangadex'
import { followingKey, queryClient, useFollowing } from '@/lib/queries'
import { commands, type FollowedManga } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'

const sourceUrl = (entry: FollowedManga) =>
  entry.source.kind === 'manga_dex'
    ? `https://mangadex.org/title/${entry.source.id}`
    : entry.source.url
const languageVariants: Record<string, string> = { 'zh-hk': 'zh-Hant', 'es-la': 'es-419' }

export function FollowingPage({ active }: { active: boolean }) {
  const { t, i18n } = useTranslation()
  const following = useFollowing(active)
  const [url, setUrl] = useState('')
  const [language, setLanguage] = useState('en')
  const [filter, setFilter] = useState('')
  const [updatesOnly, setUpdatesOnly] = useState(false)
  const [page, setPage] = useState(0)
  const [pending, setPending] = useState<string | null>(null)
  const [error, setError] = useState('')
  const [creating, setCreating] = useState<FollowedManga | null>(null)
  const generation = useRef(0)
  const scanning = useRef(false)
  const stopAfterCurrent = useRef(false)
  const [stopping, setStopping] = useState(false)
  const name = (code: string) =>
    new Intl.DisplayNames([i18n.language || 'en'], { type: 'language' }).of(
      languageVariants[code] ?? code,
    ) ?? code
  const scan = useCallback(async (entries: FollowedManga[], refresh: boolean) => {
    if (scanning.current) return
    scanning.current = true
    stopAfterCurrent.current = false
    setStopping(false)
    const token = ++generation.current
    setError('')
    try {
      for (const entry of entries) {
        if (token !== generation.current || stopAfterCurrent.current) break
        setPending(entry.key)
        const result = await commands.checkFollow(entry.key, refresh)
        queryClient.setQueryData(followingKey, result)
      }
    } catch (cause) {
      if (token === generation.current) setError(String(cause))
    } finally {
      scanning.current = false
      if (token === generation.current) {
        setPending(null)
        setStopping(false)
      }
    }
  }, [])
  useEffect(() => {
    if (!active || !following.isSuccess) return
    const entries = queryClient.getQueryData<FollowedManga[]>(followingKey) ?? []
    void scan(
      entries.filter(
        (entry) =>
          entry.source.kind === 'manga_dex' &&
          (!entry.checked_at || Date.now() / 1000 - entry.checked_at >= 900),
      ),
      false,
    )
    return () => {
      stopAfterCurrent.current = true
    }
  }, [active, following.isSuccess, scan])
  const entries = (following.data ?? [])
    .filter(
      (entry) =>
        (!updatesOnly || entry.new_chapters > 0) &&
        entry.title.toLocaleLowerCase().includes(filter.toLocaleLowerCase()),
    )
    .sort((a, b) => b.new_chapters - a.new_chapters || a.title.localeCompare(b.title))
  const pages = Math.max(1, Math.ceil(entries.length / 20))
  const current = Math.min(page, pages - 1)
  const visible = entries.slice(current * 20, current * 20 + 20)
  const pageNumbers = [
    ...new Set([
      0,
      ...Array.from({ length: 5 }, (_, i) => current + i - 2).filter((n) => n >= 0 && n < pages),
      pages - 1,
    ]),
  ].sort((a, b) => a - b)
  const save = (result: FollowedManga[]) => queryClient.setQueryData(followingKey, result)
  return (
    <main className='h-full overflow-y-auto p-4 md:p-6'>
      <div className='mx-auto grid max-w-7xl gap-5'>
        <header className='grid gap-2'>
          <h1 className='flex items-center gap-2 text-lg font-semibold'>
            <Bell className='size-5' />
            {t('following.title')}
          </h1>
          <p className='max-w-4xl text-sm leading-6 text-muted-foreground'>
            {t('following.description')}
          </p>
        </header>
        <form
          className='grid gap-3 rounded-lg border p-4 sm:grid-cols-[minmax(0,1fr)_12rem_auto] sm:items-end'
          onSubmit={(event) => {
            event.preventDefault()
            setError('')
            try {
              const address = new URL(url.trim())
              address.hash = ''
              if (
                !['http:', 'https:'].includes(address.protocol) ||
                address.username ||
                address.password
              )
                throw new Error(t('following.invalidUrl'))
              if (['mangadex.org', 'www.mangadex.org'].includes(address.hostname))
                throw new Error(t('following.addFromSearch'))
              setPending('add')
              void commands
                .followManga({
                  source: { kind: 'website', url: address.href },
                  title: address.href,
                  language,
                  cover_url: null,
                })
                .then((result) => {
                  save(result)
                  if (
                    result.some(
                      (entry) =>
                        entry.source.kind === 'website' &&
                        entry.source.url === address.href &&
                        entry.language === language,
                    )
                  )
                    setUrl('')
                })
                .catch((cause) => setError(String(cause)))
                .finally(() => setPending(null))
            } catch (cause) {
              setError(String(cause))
            }
          }}
        >
          <label className='grid min-w-0 gap-2 text-xs font-medium'>
            {t('following.sourceUrl')}
            <Input
              value={url}
              onChange={(event) => setUrl(event.target.value)}
              placeholder='https://…'
            />
          </label>
          <label className='grid gap-2 text-xs font-medium'>
            {t('following.language')}
            <NativeSelect value={language} onChange={(event) => setLanguage(event.target.value)}>
              {mangaDexLanguages.map((option) => (
                <NativeSelectOption key={option.id} value={option.id}>
                  {name(option.id)}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          </label>
          <Button type='submit' disabled={Boolean(pending) || !url.trim()}>
            {t(pending === 'add' ? 'following.adding' : 'following.addLink')}
          </Button>
          <p className='text-xs leading-5 text-muted-foreground sm:col-span-3'>
            {t('following.websiteHint')}
          </p>
        </form>
        <div className='flex flex-wrap items-center gap-3'>
          <Input
            className='max-w-72'
            aria-label={t('following.filter')}
            placeholder={t('following.filter')}
            value={filter}
            onChange={(event) => {
              setFilter(event.target.value)
              setPage(0)
            }}
          />
          <label className='flex items-center gap-2 text-xs'>
            <Checkbox
              checked={updatesOnly}
              onCheckedChange={(value) => {
                setUpdatesOnly(value === true)
                setPage(0)
              }}
            />
            {t('following.updatesOnly')}
          </label>
          <Button
            size='sm'
            variant='outline'
            disabled={Boolean(pending) || !following.data?.length}
            onClick={() => void scan(following.data ?? [], true)}
          >
            <RefreshCw className='size-3.5' />
            {t('following.checkAll')}
          </Button>
          {pending && pending !== 'add' && (
            <Button
              size='sm'
              variant='ghost'
              disabled={stopping}
              onClick={() => {
                stopAfterCurrent.current = true
                setStopping(true)
              }}
            >
              <Square className='size-3' />
              {t(stopping ? 'activity.stopping' : 'following.stopQueue')}
            </Button>
          )}
        </div>
        {(error || following.error) && (
          <p role='alert' className='max-h-32 overflow-y-auto text-sm break-words text-destructive'>
            {error || String(following.error)}
          </p>
        )}
        {following.isPending ? (
          <p role='status'>{t('following.loading')}</p>
        ) : entries.length === 0 ? (
          <p className='rounded-lg border border-dashed p-8 text-center text-sm text-muted-foreground'>
            {t('following.empty')}
          </p>
        ) : (
          <div className='grid gap-3'>
            {visible.map((entry) => (
              <article key={entry.key} className='flex min-w-0 gap-4 rounded-lg border p-4'>
                {entry.cover_url && (
                  <MangaDexCover
                    key={entry.cover_url}
                    url={entry.cover_url}
                    title={entry.title}
                    active={active}
                    className='h-28 w-20'
                  />
                )}
                <div className='grid min-w-0 flex-1 gap-2'>
                  <div className='flex flex-wrap items-start gap-2'>
                    <h2 className='min-w-0 text-sm leading-6 font-semibold break-words'>
                      {entry.title}
                    </h2>
                    {entry.new_chapters > 0 && (
                      <span className='shrink-0 rounded bg-primary/15 px-2 py-1 text-xs text-primary'>
                        {t('following.newChapters', { count: entry.new_chapters })}
                      </span>
                    )}
                  </div>
                  <p className='text-xs text-muted-foreground'>
                    {entry.source.kind === 'manga_dex'
                      ? 'MangaDex'
                      : new URL(entry.source.url).hostname}{' '}
                    · {name(entry.language)}
                  </p>
                  <div className='flex flex-wrap gap-x-5 gap-y-1 text-xs'>
                    <span>{t('following.latest', { chapter: entry.latest ?? '—' })}</span>
                    <span>{t('following.available', { count: entry.chapters })}</span>
                    <span className='text-muted-foreground'>
                      {entry.checked_at
                        ? t('following.checked', {
                            date: new Date(entry.checked_at * 1000).toLocaleString(i18n.language),
                          })
                        : t('following.noBaseline')}
                    </span>
                  </div>
                  {entry.error && (
                    <p
                      role='alert'
                      className='max-h-24 overflow-y-auto text-xs break-words text-destructive'
                    >
                      {entry.error}
                    </p>
                  )}
                  <div className='flex flex-wrap gap-2'>
                    <Button
                      size='sm'
                      variant='outline'
                      disabled={Boolean(pending)}
                      onClick={() => void scan([entry], true)}
                    >
                      <RefreshCw
                        className={`size-3 ${pending === entry.key ? 'animate-spin' : ''}`}
                      />
                      {t(pending === entry.key ? 'following.checking' : 'following.check')}
                    </Button>
                    {entry.new_chapters > 0 && (
                      <Button
                        size='sm'
                        variant='outline'
                        onClick={() =>
                          void commands
                            .markFollowSeen(entry.key)
                            .then(save)
                            .catch((cause) => setError(String(cause)))
                        }
                      >
                        <Check className='size-3' />
                        {t('following.markSeen')}
                      </Button>
                    )}
                    <Button size='sm' variant='outline' onClick={() => setCreating(entry)}>
                      <FolderPlus className='size-3' />
                      {t('mangadex.createProject')}
                    </Button>
                    <Button
                      size='sm'
                      variant='ghost'
                      onClick={() =>
                        void openUrl(sourceUrl(entry)).catch((cause) => setError(String(cause)))
                      }
                    >
                      <ExternalLink className='size-3' />
                      {t('following.openSource')}
                    </Button>
                    <Button
                      size='sm'
                      variant='ghost'
                      onClick={() =>
                        void commands
                          .unfollowManga(entry.key)
                          .then(save)
                          .catch((cause) => setError(String(cause)))
                      }
                    >
                      <Trash2 className='size-3' />
                      {t('following.remove')}
                    </Button>
                  </div>
                </div>
              </article>
            ))}
          </div>
        )}
        {pages > 1 && (
          <nav aria-label={t('mangadex.pagination')} className='flex flex-wrap items-center gap-1'>
            <Button
              size='icon-sm'
              variant='outline'
              disabled={current === 0}
              aria-label={t('mangadex.previousResults')}
              onClick={() => setPage(current - 1)}
            >
              <ChevronLeft />
            </Button>
            {pageNumbers.map((n, index) => (
              <span key={n} className='flex items-center gap-1'>
                {index > 0 && n - pageNumbers[index - 1] > 1 && <span className='px-2'>…</span>}
                <Button
                  size='sm'
                  variant={n === current ? 'default' : 'outline'}
                  aria-current={n === current ? 'page' : undefined}
                  onClick={() => setPage(n)}
                >
                  {n + 1}
                </Button>
              </span>
            ))}
            <Button
              size='icon-sm'
              variant='outline'
              disabled={current === pages - 1}
              aria-label={t('mangadex.nextResults')}
              onClick={() => setPage(current + 1)}
            >
              <ChevronRight />
            </Button>
          </nav>
        )}
      </div>
      <SourceImportDialog
        open={creating !== null}
        onOpenChange={(open) => {
          if (!open) setCreating(null)
        }}
        manga={
          creating
            ? {
                title: creating.title,
                url: sourceUrl(creating),
                language: creating.language,
                tags: [],
              }
            : undefined
        }
      />
    </main>
  )
}
