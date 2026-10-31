'use client'

import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import type { SourceChapterPreview } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Checkbox } from '@koharu/ui/components/checkbox'
import { Input } from '@koharu/ui/components/input'
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'

export function SourceChapterPicker({
  chapters,
  selected,
  onChange,
}: {
  chapters: SourceChapterPreview[]
  selected: string[]
  onChange: (urls: string[]) => void
}) {
  const { t } = useTranslation()
  const [mode, setMode] = useState('all')
  const [from, setFrom] = useState(String(chapters[0].number))
  const [to, setTo] = useState(String(chapters.at(-1)!.number))
  const [page, setPage] = useState(0)
  const checked = new Set(selected)
  const range = (start: string, end: string) => {
    const a = Number(start),
      b = Number(end)
    onChange(
      start.trim() && end.trim() && Number.isFinite(a) && Number.isFinite(b) && a <= b
        ? chapters
            .filter((chapter) => chapter.number >= a && chapter.number <= b)
            .map((chapter) => chapter.url)
        : [],
    )
  }
  return (
    <section
      aria-label={t('sourceImport.chapterSelection')}
      className='grid gap-3 rounded-lg border p-3'
    >
      <div className='flex flex-wrap items-end gap-3'>
        <label className='grid min-w-44 flex-1 gap-1 text-xs'>
          {t('sourceImport.chapterSelection')}
          <NativeSelect
            value={mode}
            onChange={(event) => {
              const value = event.target.value
              setMode(value)
              setPage(0)
              if (value === 'all') onChange(chapters.map((chapter) => chapter.url))
              else if (value === 'range') range(from, to)
              else onChange([])
            }}
          >
            <NativeSelectOption value='all'>{t('sourceImport.allChapters')}</NativeSelectOption>
            <NativeSelectOption value='range'>{t('sourceImport.chapterRange')}</NativeSelectOption>
            <NativeSelectOption value='selected'>
              {t('sourceImport.selectChapters')}
            </NativeSelectOption>
          </NativeSelect>
        </label>
        <Button
          type='button'
          variant='outline'
          size='sm'
          onClick={() => {
            setMode('selected')
            setPage(0)
            onChange([chapters[0].url])
          }}
        >
          {t('sourceImport.firstChapter')}
        </Button>
      </div>
      {mode === 'range' && (
        <div className='grid grid-cols-2 gap-3'>
          <label className='grid gap-1 text-xs'>
            {t('sourceImport.chapterFrom')}
            <Input
              type='number'
              step='any'
              value={from}
              onChange={(event) => {
                setFrom(event.target.value)
                range(event.target.value, to)
              }}
            />
          </label>
          <label className='grid gap-1 text-xs'>
            {t('sourceImport.chapterTo')}
            <Input
              type='number'
              step='any'
              value={to}
              onChange={(event) => {
                setTo(event.target.value)
                range(from, event.target.value)
              }}
            />
          </label>
        </div>
      )}
      {mode === 'selected' && (
        <>
          <div
            className='grid max-h-52 gap-1 overflow-y-auto'
            aria-label={t('sourceImport.selectChapters')}
          >
            {chapters.slice(page * 50, page * 50 + 50).map((chapter) => (
              <label
                key={chapter.url}
                className='flex items-center gap-2 rounded px-1 py-1.5 text-xs'
              >
                <Checkbox
                  checked={checked.has(chapter.url)}
                  onCheckedChange={(value) =>
                    onChange(
                      value
                        ? [...selected, chapter.url]
                        : selected.filter((url) => url !== chapter.url),
                    )
                  }
                />
                {chapter.title}
              </label>
            ))}
          </div>
          {chapters.length > 50 && (
            <div className='flex items-center justify-between gap-2 text-xs'>
              <Button
                type='button'
                size='sm'
                variant='outline'
                disabled={page === 0}
                onClick={() => setPage(page - 1)}
              >
                {t('sourceImport.previousChapters')}
              </Button>
              <span>
                {page + 1} / {Math.ceil(chapters.length / 50)}
              </span>
              <Button
                type='button'
                size='sm'
                variant='outline'
                disabled={(page + 1) * 50 >= chapters.length}
                onClick={() => setPage(page + 1)}
              >
                {t('sourceImport.nextChapters')}
              </Button>
            </div>
          )}
        </>
      )}
      <p role='status' className='text-xs font-medium'>
        {t('sourceImport.chapterCount', { count: selected.length, total: chapters.length })}
      </p>
      <p className='text-xs leading-5 text-muted-foreground'>{t('sourceImport.sizeUnknown')}</p>
    </section>
  )
}
