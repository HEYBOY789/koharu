'use client'

import { useQuery } from '@tanstack/react-query'
import { openUrl } from '@tauri-apps/plugin-opener'
import { BookOpen, ExternalLink, FolderPlus } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { queryClient } from '@/lib/queries'
import {
  commands,
  type MangaDexCacheMode,
  type MangaDexChapterRequest,
  type MangaDexComparison,
  type MangaDexEntry,
  type MangaDexMetadata,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'

let chapterLookup: Promise<void> = Promise.resolve()

async function lookupChapters(input: MangaDexChapterRequest, signal: AbortSignal) {
  const previous = chapterLookup
  let release!: () => void
  chapterLookup = new Promise<void>((resolve) => {
    release = resolve
  })
  await previous
  try {
    signal.throwIfAborted()
    return await commands.getMangadexChapters(input)
  } finally {
    release()
  }
}

function ChapterStats({
  manga,
  comparison,
  reference,
  target,
  languageName,
}: {
  manga: MangaDexEntry
  comparison?: MangaDexComparison
  reference: string
  target: string
  languageName: (code: string) => string
}) {
  const { t } = useTranslation()
  const source = comparison?.reference_language ?? reference
  const sourceLabel =
    source === 'auto'
      ? t('mangadex.auto')
      : languageName(source === 'original' ? manga.original_language : source)
  const original = comparison?.chapters.find((item) => item.language === manga.original_language)
  const total = original?.count === 0 && comparison?.total_available != null
  const columns = [
    {
      id: 'original',
      label: total
        ? t('mangadex.totalOnMangaDex')
        : t('mangadex.originalChapters', { language: languageName(manga.original_language) }),
      summary: total
        ? { latest: comparison.latest_available, count: comparison.total_available }
        : original,
    },
    {
      id: 'reference',
      label: sourceLabel,
      summary: comparison?.chapters.find((item) => item.language === source),
    },
    {
      id: 'translation',
      label: languageName(target),
      summary: comparison?.chapters.find((item) => item.language === target),
    },
  ]
  return (
    <dl className='grid grid-cols-3 grid-rows-[auto_auto_auto] gap-x-4 gap-y-1 rounded-md bg-muted/50 p-2 text-xs'>
      {columns.map(({ id, label, summary }) => (
        <div key={id} className='row-span-3 grid min-w-0 grid-rows-subgrid gap-y-1'>
          <dt className='leading-5 break-words text-muted-foreground' title={label}>
            {label}
          </dt>
          <dd className='truncate leading-5 font-medium' title={summary?.latest ?? undefined}>
            {summary
              ? summary.latest
                ? t('mangadex.chapterNumber', { number: summary.latest })
                : t(summary.count ? 'mangadex.unnumberedLabel' : 'mangadex.noChapters')
              : '…'}
          </dd>
          <dd className='truncate leading-5 text-muted-foreground'>
            {summary ? t('mangadex.chapterCount', { count: summary.count }) : '…'}
          </dd>
        </div>
      ))}
    </dl>
  )
}

export function MangaDexResultCard({
  manga,
  reference,
  target,
  cache,
  generation,
  active,
  languageName,
  metadata: sortMetadata,
  metadataLoading,
  sort,
  onCreate,
}: {
  manga: MangaDexEntry
  reference: string
  target: string
  cache: MangaDexCacheMode
  generation: number
  active: boolean
  languageName: (code: string) => string
  metadata?: MangaDexMetadata
  metadataLoading: boolean
  sort: string
  onCreate: () => void
}) {
  const { t, i18n } = useTranslation()
  const card = useRef<HTMLElement>(null)
  const [seen, setSeen] = useState(false)
  const [imageFailed, setImageFailed] = useState(false)
  const [linkError, setLinkError] = useState('')
  useEffect(() => {
    if (!active || seen || !card.current || typeof IntersectionObserver === 'undefined') return
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          setSeen(true)
          observer.disconnect()
        }
      },
      { rootMargin: '150px' },
    )
    observer.observe(card.current)
    return () => observer.disconnect()
  }, [active, seen])
  const input = {
    id: manga.id,
    original_language: manga.original_language,
    reference,
    target,
    chapter_numbers_reset: manga.chapter_numbers_reset,
    cache,
  }
  const chapters = useQuery(
    {
      queryKey: ['mangadex-chapters', input, generation],
      queryFn: ({ signal }) => lookupChapters(input, signal),
      enabled: (query) => active && seen && query.state.errorUpdateCount === 0,
      initialData: manga.comparison ?? undefined,
      // The search generation owns refresh. Scrolling must not restart a completed/failed lookup.
      staleTime: Infinity,
      retry: false,
      retryOnMount: false,
      refetchOnMount: false,
      refetchOnWindowFocus: false,
      refetchOnReconnect: false,
    },
    queryClient,
  )
  const metadata = [
    manga.author,
    manga.artist !== manga.author ? manga.artist : '',
    languageName(manga.original_language),
    manga.year,
  ]
    .filter(Boolean)
    .join(' · ')
  const comparison = chapters.data
  const unavailable = t('mangadex.unknownMetadata')
  const dateLabel = (value: string | null | undefined) => {
    const date = value ? new Date(value) : null
    return date && Number.isFinite(date.getTime())
      ? date.toLocaleString(i18n.language, { dateStyle: 'medium', timeStyle: 'short' })
      : unavailable
  }
  const sortFields = [
    {
      field: 'rating',
      label: t('mangadex.weightedRating'),
      help: t('mangadex.ratingHelp'),
      value: metadataLoading
        ? '…'
        : sortMetadata?.rating == null
          ? unavailable
          : `${sortMetadata.rating.toLocaleString(i18n.language, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} / 10`,
    },
    {
      field: 'followedCount',
      label: t('mangadex.follows'),
      value: metadataLoading
        ? '…'
        : sortMetadata?.follows == null
          ? unavailable
          : sortMetadata.follows.toLocaleString(i18n.language),
    },
    {
      field: 'latestUploadedChapter',
      label: t('mangadex.latestUpload'),
      value: metadataLoading ? '…' : dateLabel(sortMetadata?.latest_upload_at),
    },
    { field: 'createdAt', label: t('mangadex.addedAt'), value: dateLabel(manga.created_at) },
  ]
  const sortedField = sort.split('.')[0] || 'latestUploadedChapter'
  const names = {
    target: languageName(comparison?.target_language ?? target),
    reference: languageName(
      comparison?.reference_language ??
        (reference === 'original' ? manga.original_language : reference),
    ),
  }
  return (
    <article ref={card} className='min-w-0 space-y-3 rounded-lg border p-3'>
      <div className='flex items-start gap-3'>
        {manga.cover_url && !imageFailed ? (
          <img
            src={manga.cover_url}
            alt={t('mangadex.coverFor', { title: manga.title })}
            width={96}
            height={144}
            loading='eager'
            decoding='async'
            fetchPriority='low'
            referrerPolicy='no-referrer'
            className='h-36 w-24 shrink-0 rounded-md bg-muted object-contain'
            onError={() => setImageFailed(true)}
          />
        ) : (
          <div className='flex h-36 w-24 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground'>
            <BookOpen className='size-6' aria-label={t('mangadex.noCover')} />
          </div>
        )}
        <div className='min-w-0 flex-1 space-y-2'>
          <h3 className='line-clamp-2 text-sm font-semibold break-words' title={manga.title}>
            {manga.title}
          </h3>
          <p className='line-clamp-2 text-[11px] break-words text-muted-foreground'>{metadata}</p>
          <p className='line-clamp-2 text-[11px] text-muted-foreground'>
            {[
              manga.publication_status && t(`mangadex.${manga.publication_status}`),
              manga.content_rating && t(`mangadex.${manga.content_rating}`),
            ]
              .filter(Boolean)
              .join(' · ')}
          </p>
          <div className='space-y-1'>
            <div className='min-h-6 text-[10px]'>
              {comparison?.status && (
                <span className='inline-block max-w-full rounded bg-muted px-2 py-1'>
                  <span className='line-clamp-2'>{t(`mangadex.${comparison.status}`, names)}</span>
                </span>
              )}
            </div>
            <div className='min-h-8 text-[11px] leading-4 text-muted-foreground'>
              {chapters.error ? (
                <div className='flex items-start gap-2'>
                  <p
                    role='alert'
                    className='line-clamp-2 min-w-0 flex-1 break-words text-destructive'
                    title={String(chapters.error)}
                  >
                    {String(chapters.error)}
                  </p>
                  <Button
                    type='button'
                    variant='outline'
                    size='sm'
                    disabled={chapters.isFetching || !active}
                    onClick={() => void chapters.refetch()}
                  >
                    {t('mangadex.retryChapters')}
                  </Button>
                </div>
              ) : (
                <p className='line-clamp-2'>
                  {chapters.isFetching
                    ? t('mangadex.loadingChapters')
                    : comparison?.missing
                      ? t('mangadex.missingBetween', { ...names, count: comparison.missing })
                      : t('mangadex.availabilityNote')}
                </p>
              )}
            </div>
          </div>
        </div>
      </div>
      <dl className='grid grid-cols-2 gap-x-4 gap-y-1 border-t pt-3 text-xs'>
        {sortFields.map(({ field, label, value, help }) => (
          <div key={field} className='row-span-2 grid min-w-0 grid-rows-subgrid gap-y-1 pb-2'>
            <dt
              title={help}
              className={`leading-5 break-words ${field === sortedField ? 'text-primary' : 'text-muted-foreground'}`}
            >
              {label}
            </dt>
            <dd
              className={`leading-5 break-words ${field === sortedField ? 'font-semibold text-primary' : 'font-medium'}`}
            >
              {value}
            </dd>
          </div>
        ))}
      </dl>
      <ChapterStats
        manga={manga}
        comparison={comparison}
        reference={reference}
        target={target}
        languageName={languageName}
      />
      {manga.description && (
        <p className='line-clamp-3 text-xs leading-5 text-muted-foreground'>{manga.description}</p>
      )}
      {manga.tags.length > 0 && (
        <div className='space-y-2'>
          <p className='text-xs font-medium'>{t('mangadex.tags')}</p>
          <ul aria-label={t('mangadex.tags')} className='flex flex-wrap gap-1.5'>
            {manga.tags.map((tag) => (
              <li
                key={tag}
                className='rounded border px-2 py-1 text-[11px] leading-4 text-muted-foreground'
              >
                {tag}
              </li>
            ))}
          </ul>
        </div>
      )}
      <div className='flex flex-wrap gap-2'>
        <Button type='button' size='sm' onClick={onCreate}>
          <FolderPlus className='size-3' />
          {t('mangadex.createProject')}
        </Button>
        <Button
          type='button'
          variant='outline'
          size='sm'
          onClick={() => void openUrl(manga.url).catch((cause) => setLinkError(String(cause)))}
        >
          <ExternalLink className='size-3' />
          {t('mangadex.open')}
        </Button>
      </div>
      {linkError && (
        <p role='alert' className='text-xs break-words text-destructive'>
          {linkError}
        </p>
      )}
    </article>
  )
}
