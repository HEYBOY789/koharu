'use client'

import { BookOpen } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { commands } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { cn } from '@koharu/ui/lib/utils'

const coverRetryDelays = [2000, 5000]

export function MangaDexCover({
  url,
  title,
  active,
  className,
}: {
  url: string | null
  title: string
  active: boolean
  className?: string
}) {
  const { t } = useTranslation()
  const [attempt, setAttempt] = useState(0)
  const [source, setSource] = useState<string | null>(null)
  const requested = useRef(-1)
  const [status, setStatus] = useState<'loading' | 'loaded' | 'error'>('loading')
  useEffect(
    () => () => {
      if (source) URL.revokeObjectURL(source)
    },
    [source],
  )
  useEffect(() => {
    if (!url || !active || requested.current === attempt) return
    requested.current = attempt
    let cancelled = false
    let pending = true
    setSource(null)
    setStatus('loading')
    void commands
      .getMangadexCover(url)
      .then((bytes) => {
        pending = false
        if (cancelled) return
        setSource(URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: 'image/jpeg' })))
      })
      .catch(() => {
        pending = false
        if (!cancelled) setStatus('error')
      })
    return () => {
      cancelled = true
      if (pending) requested.current = -1
    }
  }, [active, attempt, url])
  const retrying = status === 'error' && attempt < coverRetryDelays.length
  useEffect(() => {
    if (!url || !active || !retrying) return
    const timer = window.setTimeout(() => {
      setAttempt((value) => value + 1)
      setStatus('loading')
    }, coverRetryDelays[attempt])
    return () => window.clearTimeout(timer)
  }, [active, attempt, retrying, url])
  return (
    <div
      className={cn(
        'flex h-36 w-24 shrink-0 flex-col items-center justify-center gap-2 overflow-hidden rounded-md bg-muted text-center text-muted-foreground',
        className,
      )}
    >
      {source && status !== 'error' ? (
        <img
          key={attempt}
          src={source}
          alt={t('mangadex.coverFor', { title })}
          width={96}
          height={144}
          loading='eager'
          decoding='async'
          fetchPriority='low'
          referrerPolicy='no-referrer'
          className='h-full w-full object-contain'
          onLoad={() => setStatus('loaded')}
          onError={() => setStatus('error')}
        />
      ) : (
        <>
          <BookOpen className='size-6 shrink-0' aria-label={t('mangadex.noCover')} />
          {url && (
            <div className='grid w-full gap-2 px-2'>
              <p className='text-[10px] leading-3.5'>
                {t(
                  status === 'loading'
                    ? 'mangadex.loadingCover'
                    : retrying && active
                      ? 'mangadex.retryingCover'
                      : 'mangadex.coverFailed',
                )}
              </p>
              {status === 'error' && !retrying && (
                <Button
                  type='button'
                  variant='outline'
                  size='xs'
                  className='h-auto min-h-6 max-w-full px-1.5 py-1 text-[10px] leading-3.5 whitespace-normal'
                  aria-label={t('mangadex.retryCoverFor', { title })}
                  disabled={!active}
                  onClick={() => {
                    setAttempt(0)
                    setStatus('loading')
                  }}
                >
                  {t('mangadex.retryCover')}
                </Button>
              )}
            </div>
          )}
        </>
      )}
    </div>
  )
}
