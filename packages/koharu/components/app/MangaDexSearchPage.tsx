'use client'

import { useQuery } from '@tanstack/react-query'
import { openUrl } from '@tauri-apps/plugin-opener'
import {
  ChevronLeft,
  ChevronRight,
  ExternalLink,
  LoaderCircle,
  RefreshCw,
  Search,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'

import { MangaDexMultiSelect, MangaDexPeople } from '@/components/app/MangaDexFilters'
import { MangaDexResultCard } from '@/components/app/MangaDexResultCard'
import { SourceImportDialog } from '@/components/app/SourceImportDialog'
import { mangaDexDefaults, mangaDexLanguages, mangaDexSorts } from '@/lib/mangadex'
import { queryClient } from '@/lib/queries'
import {
  commands,
  type MangaDexOption,
  type MangaDexPage,
  type MangaDexSearch,
  type MangaDexEntry,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'
import {
  Pagination,
  PaginationContent,
  PaginationEllipsis,
  PaginationItem,
} from '@koharu/ui/components/pagination'

type ResultBatch = MangaDexPage & { chapterCache: MangaDexSearch['cache']; generation: number }
type SearchResults = {
  query: MangaDexSearch
  page: number
  batches: Record<number, ResultBatch>
}

export function MangaDexSearchPage({ active }: { active: boolean }) {
  const { t, i18n } = useTranslation()
  const [input, setInput] = useState(mangaDexDefaults)
  const [authors, setAuthors] = useState<MangaDexOption[]>([])
  const [artists, setArtists] = useState<MangaDexOption[]>([])
  const [tags, setTags] = useState<MangaDexOption[]>([])
  const [tagSearch, setTagSearch] = useState('')
  const [tagError, setTagError] = useState('')
  const [result, setResult] = useState<SearchResults | null>(null)
  const [pageInput, setPageInput] = useState('1')
  const [creating, setCreating] = useState<MangaDexEntry | null>(null)
  const resultsSection = useRef<HTMLElement>(null)
  const scrollArea = useRef<HTMLDivElement>(null)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')
  const request = useRef(0)
  const query = {
    ...input,
    title: input.title.trim(),
    authors: authors.map((item) => item.id),
    artists: artists.map((item) => item.id),
  }
  const appliedQuery = result?.query ?? query
  const filtersChanged = result !== null && JSON.stringify(query) !== JSON.stringify(result.query)
  const pageSize = appliedQuery.filter === 'any' ? 20 : 10
  const batchSize = appliedQuery.filter === 'any' ? 100 : 10
  const currentPage = result?.page ?? 0
  const pageStart = currentPage * pageSize
  const batchOffset = Math.floor(pageStart / batchSize) * batchSize
  const visible = result?.batches[batchOffset] ?? null
  const resultPages = Math.max(1, Math.ceil(Math.min(visible?.total ?? 0, 10000) / pageSize))
  const localStart = appliedQuery.filter === 'any' ? pageStart - batchOffset : 0
  const shown = visible?.manga.slice(localStart, localStart + pageSize) ?? []
  const nearbyStart = Math.max(0, Math.min(currentPage - 2, resultPages - 5))
  const nearbyPages = Array.from({ length: Math.min(5, resultPages) }, (_, i) => nearbyStart + i)
  const numberedPages = [...new Set([0, ...nearbyPages, resultPages - 1])].sort((a, b) => a - b)
  const pageNumbers: Array<number | string> = []
  numberedPages.forEach((page, index) => {
    const previous = numberedPages[index - 1]
    if (index && page - previous === 2) pageNumbers.push(previous + 1)
    else if (index && page - previous > 2) pageNumbers.push(`gap-${page}`)
    pageNumbers.push(page)
  })
  const totalItems = Math.min(visible?.total ?? 0, 10000)
  const pageItems =
    appliedQuery.filter === 'any' ? shown.length : Math.min(visible?.scanned ?? 0, pageSize)
  const rangeEnd = pageItems ? Math.min(pageStart + pageItems, totalItems) : 0
  const rangeStart = pageItems ? pageStart + 1 : 0
  const metadataInput = useMemo(
    () =>
      visible?.manga.map((manga) => ({
        id: manga.id,
        latest_uploaded_chapter: manga.latest_uploaded_chapter,
      })) ?? [],
    [visible?.manga],
  )
  const metadataCache = visible?.chapterCache ?? input.cache
  const metadata = useQuery(
    {
      queryKey: ['mangadex-metadata', metadataInput, metadataCache, visible?.generation],
      queryFn: ({ signal }) => {
        signal.throwIfAborted()
        return commands.getMangadexMetadata(metadataInput, metadataCache)
      },
      enabled: active && !pending && metadataInput.length > 0,
      staleTime: Infinity,
      retry: false,
      retryOnMount: false,
      refetchOnMount: false,
      refetchOnWindowFocus: false,
      refetchOnReconnect: false,
    },
    queryClient,
  )
  const languages = useMemo(() => {
    const names = new Intl.DisplayNames([i18n.language || 'en'], { type: 'language' })
    const variants: Record<string, string> = { 'zh-hk': 'zh-Hant', 'es-la': 'es-419' }
    return mangaDexLanguages.map((language) => ({
      id: language.id,
      name: names.of(variants[language.id] ?? language.id) ?? language.fallback,
    }))
  }, [i18n.language])
  const languageName = (code: string) =>
    languages.find((language) => language.id === code)?.name ?? code.toUpperCase()
  const change = <K extends keyof MangaDexSearch>(field: K, value: MangaDexSearch[K]) =>
    setInput((previous) => ({ ...previous, [field]: value, offset: 0 }))
  useEffect(() => {
    if (!active) {
      void queryClient.cancelQueries({ queryKey: ['mangadex-chapters'] })
      void queryClient.cancelQueries({ queryKey: ['mangadex-metadata'] })
    }
  }, [active])
  useEffect(() => {
    if (!active) return
    let current = true
    setTagError('')
    void commands
      .getMangadexOptions('tags', '', input.cache)
      .then((value) => {
        if (current) setTags(value.options)
      })
      .catch((cause) => {
        if (current) setTagError(String(cause))
      })
    return () => {
      current = false
    }
  }, [active, input.cache])
  useEffect(
    () => () => {
      request.current++
    },
    [],
  )
  async function search(refresh = false) {
    if (pending) return
    const token = ++request.current
    setPending(true)
    setError('')
    try {
      const page = await commands.searchMangadex({
        ...query,
        offset: 0,
        cache: refresh ? 'refresh' : input.cache,
      })
      if (token !== request.current) return
      setResult({
        query: { ...query, offset: 0 },
        page: 0,
        batches: {
          0: {
            ...page,
            chapterCache: refresh ? 'refresh' : input.cache,
            generation: token,
          },
        },
      })
      setPageInput('1')
    } catch (cause) {
      if (token === request.current) setError(String(cause))
    } finally {
      if (token === request.current) setPending(false)
    }
  }
  async function goToPage(nextPage: number) {
    if (
      pending ||
      !result ||
      !Number.isInteger(nextPage) ||
      nextPage < 0 ||
      nextPage >= resultPages
    )
      return
    if (nextPage === currentPage) {
      setPageInput(String(currentPage + 1))
      return
    }
    const offset = Math.floor((nextPage * pageSize) / batchSize) * batchSize
    if (result.batches[offset]) {
      setResult({ ...result, page: nextPage })
      setPageInput(String(nextPage + 1))
      setError('')
      scrollToResults()
      return
    }
    const previous = result
    const token = ++request.current
    setPending(true)
    setError('')
    try {
      const page = await commands.searchMangadex({ ...previous.query, offset })
      if (token !== request.current) return
      setResult({
        ...previous,
        page: nextPage,
        batches: {
          ...previous.batches,
          [offset]: {
            ...page,
            chapterCache: previous.query.cache,
            generation: token,
          },
        },
      })
      setPageInput(String(nextPage + 1))
      scrollToResults()
    } catch (cause) {
      if (token === request.current) setError(String(cause))
    } finally {
      if (token === request.current) setPending(false)
    }
  }
  function scrollToResults() {
    const viewport = scrollArea.current,
      results = resultsSection.current
    if (!viewport || !results) return
    viewport.scrollTop += results.getBoundingClientRect().top - viewport.getBoundingClientRect().top
  }
  const reset = () => {
    setInput(mangaDexDefaults())
    setAuthors([])
    setArtists([])
    setTagSearch('')
    setError('')
  }
  const submit = (event: FormEvent) => {
    event.preventDefault()
    void search()
  }
  const options = (values: readonly string[]) =>
    values.map((id) => ({ id, name: t(`mangadex.${id}`) }))
  const comparisonNames = {
    target: languageName(input.target),
    reference:
      input.reference === 'auto'
        ? t('mangadex.auto')
        : input.reference === 'original'
          ? t('mangadex.original')
          : languageName(input.reference),
  }
  const select = (
    field: 'sort' | 'filter',
    label: string,
    entries: Array<readonly [string, string]>,
    disabled = pending,
  ) => (
    <label className='grid min-w-0 content-start gap-2 text-xs leading-5'>
      <span>{label}</span>
      <NativeSelect
        className='w-full [&>select]:block [&>select]:h-9'
        aria-label={label}
        value={input[field]}
        disabled={disabled}
        onChange={(event) => setInput((previous) => ({ ...previous, [field]: event.target.value }))}
      >
        {entries.map(([value, name]) => (
          <NativeSelectOption
            key={value}
            value={value}
            disabled={value === 'relevance.desc' && !input.title.trim()}
          >
            {t(`mangadex.${name}`, comparisonNames)}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </label>
  )
  const cycleTag = (id: string) =>
    setInput((previous) => ({
      ...previous,
      included_tags: previous.excluded_tags.includes(id)
        ? previous.included_tags
        : previous.included_tags.includes(id)
          ? previous.included_tags.filter((tag) => tag !== id)
          : [...previous.included_tags, id],
      excluded_tags: previous.excluded_tags.includes(id)
        ? previous.excluded_tags.filter((tag) => tag !== id)
        : previous.included_tags.includes(id)
          ? [...previous.excluded_tags, id]
          : previous.excluded_tags,
    }))
  return (
    <main
      className='flex h-full min-h-0 flex-col gap-4 overflow-hidden bg-[var(--surface-canvas)] p-4 sm:p-6'
      aria-labelledby='mangadex-title'
    >
      <header className='shrink-0'>
        <h1 id='mangadex-title' className='text-lg font-semibold'>
          {t('mangadex.title')}
        </h1>
        <p className='mt-1 text-sm text-muted-foreground'>{t('mangadex.description')}</p>
      </header>
      <div ref={scrollArea} className='min-h-0 flex-1 overflow-y-auto pr-1'>
        <form onSubmit={submit} className='space-y-5'>
          <fieldset
            disabled={pending}
            className='grid items-start gap-x-4 gap-y-5 sm:grid-cols-2 lg:grid-cols-4'
          >
            <label className='grid min-w-0 content-start gap-2 text-xs leading-5 sm:col-span-2'>
              <span>{t('mangadex.query')}</span>
              <Input
                className='h-9 text-sm'
                value={input.title}
                maxLength={200}
                onChange={(event) =>
                  setInput((previous) => ({
                    ...previous,
                    title: event.target.value,
                    sort:
                      previous.sort === 'relevance.desc' && !event.target.value.trim()
                        ? ''
                        : previous.sort,
                  }))
                }
                placeholder={t('mangadex.placeholder')}
              />
            </label>
            {select('sort', t('mangadex.sort'), [...mangaDexSorts])}
            {select('filter', t('mangadex.filter'), [
              ['any', 'any'],
              ['either', 'either'],
              ['no_target', 'no_target'],
              ['behind', 'behind'],
            ])}
            {(['reference', 'target'] as const).map((field) => (
              <label
                key={field}
                className='grid min-w-0 content-start gap-2 text-xs leading-5 sm:col-span-2'
              >
                <span>
                  {t(
                    field === 'reference'
                      ? 'mangadex.referenceLanguage'
                      : 'mangadex.translationLanguage',
                  )}
                </span>
                <NativeSelect
                  className='w-full [&>select]:block [&>select]:h-9'
                  value={input[field]}
                  onChange={(event) => change(field, event.target.value)}
                >
                  {field === 'reference' && (
                    <>
                      <NativeSelectOption value='auto'>{t('mangadex.auto')}</NativeSelectOption>
                      <NativeSelectOption value='original'>
                        {t('mangadex.original')}
                      </NativeSelectOption>
                    </>
                  )}
                  {languages.map((language) => (
                    <NativeSelectOption key={language.id} value={language.id}>
                      {language.name}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </label>
            ))}
          </fieldset>
          <details className='rounded-lg border p-4'>
            <summary className='cursor-pointer text-sm font-medium'>
              {t('mangadex.advanced')}
            </summary>
            <fieldset
              disabled={pending}
              className='mt-5 grid items-start gap-x-4 gap-y-5 sm:grid-cols-2 lg:grid-cols-4'
            >
              <MangaDexMultiSelect
                label={t('mangadex.original')}
                options={languages}
                value={input.original_languages}
                onChange={(value) => change('original_languages', value)}
                disabled={pending}
              />
              <MangaDexMultiSelect
                label={t('mangadex.rating')}
                options={options(['safe', 'suggestive', 'erotica', 'pornographic'])}
                value={input.content_ratings}
                onChange={(value) => change('content_ratings', value)}
                disabled={pending}
              />
              <MangaDexMultiSelect
                label={t('mangadex.demographic')}
                options={options(['shounen', 'shoujo', 'seinen', 'josei', 'none'])}
                value={input.demographics}
                onChange={(value) => change('demographics', value)}
                disabled={pending}
              />
              <MangaDexMultiSelect
                label={t('mangadex.publication')}
                options={options(['ongoing', 'completed', 'hiatus', 'cancelled'])}
                value={input.statuses}
                onChange={(value) => change('statuses', value)}
                disabled={pending}
              />
              <MangaDexPeople
                label={t('mangadex.authors')}
                value={authors}
                onChange={setAuthors}
                cache={input.cache}
                disabled={pending || !active}
              />
              <MangaDexPeople
                label={t('mangadex.artists')}
                value={artists}
                onChange={setArtists}
                cache={input.cache}
                disabled={pending || !active}
              />
              <label className='grid min-w-0 content-start gap-2 text-xs leading-5'>
                <span>{t('mangadex.year')}</span>
                <Input
                  className='h-9 text-sm'
                  type='number'
                  min={1000}
                  max={9999}
                  step={1}
                  value={input.year ?? ''}
                  onChange={(event) =>
                    change('year', event.target.value ? Number(event.target.value) : null)
                  }
                  placeholder={t('mangadex.all')}
                />
              </label>
              <div className='flex min-w-0 flex-col gap-2.5'>
                <MangaDexMultiSelect
                  label={t('mangadex.translated')}
                  options={languages}
                  value={input.translated_languages}
                  onChange={(value) => change('translated_languages', value)}
                  disabled={pending || !input.has_chapters}
                />
                <label className='flex items-center gap-2.5 text-xs leading-5'>
                  <Checkbox
                    checked={input.has_chapters}
                    onCheckedChange={(checked) => change('has_chapters', checked === true)}
                  />
                  <span>{t('mangadex.hasChapters')}</span>
                </label>
              </div>
              <div className='space-y-3 border-t pt-4 sm:col-span-2 lg:col-span-4'>
                <div className='flex flex-wrap items-center gap-3'>
                  <p className='text-xs font-medium'>{t('mangadex.tags')}</p>
                  <label className='flex items-center gap-2 text-xs'>
                    <Checkbox
                      checked={input.include_all}
                      onCheckedChange={(checked) => change('include_all', checked === true)}
                    />
                    {t('mangadex.includeAll')}
                  </label>
                  <label className='flex items-center gap-2 text-xs'>
                    <Checkbox
                      checked={input.exclude_all}
                      onCheckedChange={(checked) => change('exclude_all', checked === true)}
                    />
                    {t('mangadex.excludeAll')}
                  </label>
                  <Button
                    type='button'
                    variant='ghost'
                    size='sm'
                    disabled={pending}
                    onClick={() =>
                      setInput((previous) => ({
                        ...previous,
                        included_tags: [],
                        excluded_tags: [],
                      }))
                    }
                  >
                    {t('mangadex.clearTags')}
                  </Button>
                </div>
                <p className='text-[11px] text-muted-foreground'>{t('mangadex.tagHint')}</p>
                {tagError && (
                  <p role='alert' className='text-xs break-words text-destructive'>
                    {tagError}
                  </p>
                )}
                <Input
                  aria-label={t('mangadex.findTag')}
                  value={tagSearch}
                  onChange={(event) => setTagSearch(event.target.value)}
                  placeholder={t('mangadex.findTag')}
                />
                <div className='max-h-52 space-y-2 overflow-y-auto rounded border p-2'>
                  {['content', 'format', 'genre', 'theme'].map((group) => (
                    <div key={group} className='space-y-1'>
                      <p className='text-[11px] font-medium text-muted-foreground'>
                        {t(`mangadex.group_${group}`)}
                      </p>
                      <div className='flex flex-wrap gap-1'>
                        {tags
                          .filter(
                            (tag) =>
                              tag.group === group &&
                              (tag.name.toLowerCase().includes(tagSearch.toLowerCase()) ||
                                input.included_tags.includes(tag.id) ||
                                input.excluded_tags.includes(tag.id)),
                          )
                          .map((tag) => {
                            const state = input.included_tags.includes(tag.id)
                              ? 'include'
                              : input.excluded_tags.includes(tag.id)
                                ? 'exclude'
                                : 'neutral'
                            return (
                              <Button
                                key={tag.id}
                                type='button'
                                size='sm'
                                variant={state === 'neutral' ? 'outline' : 'secondary'}
                                className={
                                  state === 'include'
                                    ? 'border border-emerald-600 text-emerald-700 dark:text-emerald-300'
                                    : state === 'exclude'
                                      ? 'border border-red-500 text-red-700 dark:text-red-300'
                                      : ''
                                }
                                aria-label={`${tag.name}: ${t(`mangadex.${state}`)}`}
                                onClick={() => cycleTag(tag.id)}
                              >
                                {state === 'include' ? '+' : state === 'exclude' ? '−' : '·'}{' '}
                                {tag.name}
                              </Button>
                            )
                          })}
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            </fieldset>
          </details>
          <div className='flex flex-wrap items-center gap-2'>
            <Button type='submit' disabled={pending}>
              {pending ? (
                <LoaderCircle className='size-3.5 animate-spin' />
              ) : (
                <Search className='size-3.5' />
              )}
              {t('mangadex.search')}
            </Button>
            <Button
              type='button'
              variant='outline'
              disabled={pending || input.cache === 'offline'}
              onClick={() => void search(true)}
            >
              <RefreshCw className='size-3.5' />
              {t('mangadex.refresh')}
            </Button>
            <Button type='button' variant='ghost' disabled={pending} onClick={reset}>
              {t('mangadex.reset')}
            </Button>
            <label className='ml-auto flex items-center gap-2 text-xs'>
              <Checkbox
                disabled={pending}
                checked={input.cache === 'offline'}
                onCheckedChange={(checked) => change('cache', checked ? 'offline' : 'online')}
              />
              {t('mangadex.offline')}
            </label>
          </div>
        </form>
        <p className='my-3 text-[11px] leading-4 text-muted-foreground'>
          {t(input.filter === 'any' ? 'mangadex.listNote' : 'mangadex.note')}
        </p>
        <p className='mb-3 text-[11px] leading-4 text-muted-foreground'>{t('mangadex.sortHelp')}</p>
        <p className='mb-3 text-[11px] leading-4 text-muted-foreground'>
          {t('mangadex.cacheHelp')}
        </p>
        {filtersChanged && (
          <p role='status' className='mb-3 rounded-md border px-3 py-2 text-xs'>
            {t('mangadex.filtersChanged')}
          </p>
        )}
        {pending && (
          <p role='status' className='mb-3 text-xs text-muted-foreground'>
            {t('mangadex.checking')}
          </p>
        )}
        {error && (
          <p role='alert' className='mb-3 text-xs break-words text-destructive'>
            {error}
          </p>
        )}
        {visible && (
          <section ref={resultsSection} aria-label={t('mangadex.results')} className='space-y-3'>
            <div className='flex flex-wrap justify-between gap-2 text-xs text-muted-foreground'>
              <p>
                {t('mangadex.counts', {
                  scanned: visible.scanned,
                  count: shown.length,
                  total: visible.total,
                })}
              </p>
              <p>
                {t('mangadex.checked', {
                  time: new Date(visible.checked_at * 1000).toLocaleString(i18n.language),
                })}{' '}
                · {t('mangadex.requests', { count: visible.network_requests })}
              </p>
            </div>
            {visible.manga.length === 0 && (
              <p className='rounded-md border p-4 text-sm'>{t('mangadex.empty')}</p>
            )}
            {metadata.error && (
              <div className='flex flex-wrap items-center gap-2 text-xs'>
                <p role='alert' className='text-destructive' title={String(metadata.error)}>
                  {t('mangadex.metadataError')}
                </p>
                <Button
                  type='button'
                  variant='outline'
                  size='sm'
                  disabled={metadata.isFetching || !active}
                  onClick={() => void metadata.refetch()}
                >
                  {t('mangadex.retryMetadata')}
                </Button>
              </div>
            )}
            <div className='grid gap-3 md:grid-cols-2'>
              {shown.map((manga) => (
                <MangaDexResultCard
                  key={manga.id}
                  manga={manga}
                  reference={appliedQuery.reference}
                  target={appliedQuery.target}
                  cache={visible.chapterCache}
                  generation={visible.generation}
                  active={active && !pending}
                  languageName={languageName}
                  metadata={metadata.data?.manga.find((item) => item.id === manga.id)}
                  metadataLoading={metadata.isPending && !metadata.error}
                  sort={appliedQuery.sort}
                  onCreate={() => setCreating(manga)}
                />
              ))}
            </div>
            {appliedQuery.filter !== 'any' && (
              <p className='text-xs leading-5 text-muted-foreground'>
                {t('mangadex.comparisonPages')}
              </p>
            )}
            <Pagination
              aria-label={t('mangadex.pagination')}
              className='mx-0 w-full flex-wrap items-center justify-start gap-x-3 gap-y-2'
            >
              <span className='text-xs text-muted-foreground tabular-nums'>
                {t(
                  appliedQuery.filter === 'any'
                    ? 'mangadex.itemsRange'
                    : 'mangadex.candidatesRange',
                  {
                    start: rangeStart.toLocaleString(i18n.language),
                    end: rangeEnd.toLocaleString(i18n.language),
                    total: totalItems.toLocaleString(i18n.language),
                  },
                )}
              </span>
              <PaginationContent className='gap-1'>
                <PaginationItem>
                  <Button
                    type='button'
                    size='icon'
                    variant='outline'
                    className='rounded-sm'
                    aria-label={t('mangadex.previousResults')}
                    title={t('mangadex.previousResults')}
                    disabled={pending || currentPage === 0}
                    onClick={() => void goToPage(currentPage - 1)}
                  >
                    <ChevronLeft className='size-4' />
                  </Button>
                </PaginationItem>
                {pageNumbers.map((page) => (
                  <PaginationItem key={page}>
                    {typeof page === 'string' ? (
                      <PaginationEllipsis className='w-5' />
                    ) : (
                      <Button
                        type='button'
                        size='icon'
                        variant='outline'
                        className={`min-w-8 rounded-sm px-1 text-xs tabular-nums ${page === currentPage ? 'border-primary bg-primary/10 text-primary dark:border-primary dark:bg-primary/10' : ''}`}
                        aria-label={t('mangadex.pageNumber', { page: page + 1 })}
                        aria-current={page === currentPage ? 'page' : undefined}
                        disabled={pending}
                        onClick={() => void goToPage(page)}
                      >
                        {page + 1}
                      </Button>
                    )}
                  </PaginationItem>
                ))}
                <PaginationItem>
                  <Button
                    type='button'
                    size='icon'
                    variant='outline'
                    className='rounded-sm'
                    aria-label={t('mangadex.nextResults')}
                    title={t('mangadex.nextResults')}
                    disabled={pending || currentPage === resultPages - 1}
                    onClick={() => void goToPage(currentPage + 1)}
                  >
                    <ChevronRight className='size-4' />
                  </Button>
                </PaginationItem>
              </PaginationContent>
              <form
                className='ml-auto flex items-center gap-1.5'
                onSubmit={(event) => {
                  event.preventDefault()
                  if (pageInput.trim()) void goToPage(Number(pageInput) - 1)
                }}
              >
                <label className='sr-only' htmlFor='mangadex-page-number'>
                  {t('mangadex.goToPage')}
                </label>
                <Input
                  id='mangadex-page-number'
                  type='number'
                  inputMode='numeric'
                  min={1}
                  max={resultPages}
                  step={1}
                  value={pageInput}
                  disabled={pending}
                  className='h-8 w-16 text-xs'
                  onChange={(event) => setPageInput(event.target.value)}
                />
                <Button
                  type='submit'
                  variant='outline'
                  size='sm'
                  className='h-8 rounded-sm'
                  disabled={
                    pending ||
                    !pageInput.trim() ||
                    !Number.isInteger(Number(pageInput)) ||
                    Number(pageInput) < 1 ||
                    Number(pageInput) > resultPages
                  }
                >
                  {t('mangadex.go')}
                </Button>
              </form>
              <span className='sr-only'>
                {t('mangadex.pageOf', { page: currentPage + 1, pages: resultPages })}
              </span>
            </Pagination>
            {(visible.capped || visible.total > 10000) && (
              <p className='text-xs text-muted-foreground'>{t('mangadex.capped')}</p>
            )}
          </section>
        )}
      </div>
      <div className='flex shrink-0 items-center justify-between gap-3 border-t pt-2 text-[11px] text-muted-foreground'>
        <span>{t('mangadex.credit')}</span>
        <Button
          variant='ghost'
          size='sm'
          onClick={() =>
            void openUrl('https://mangadex.org/').catch((cause) => setError(String(cause)))
          }
        >
          MangaDex <ExternalLink className='size-3' />
        </Button>
      </div>
      {creating && (
        <SourceImportDialog
          open
          onOpenChange={(open) => {
            if (!open) setCreating(null)
          }}
          manga={{
            title: creating.title,
            url: creating.url,
            tags: creating.tags,
            language:
              creating.comparison?.reference_language ??
              (appliedQuery.reference === 'auto'
                ? 'en'
                : appliedQuery.reference === 'original'
                  ? creating.original_language
                  : appliedQuery.reference),
          }}
        />
      )}
    </main>
  )
}
