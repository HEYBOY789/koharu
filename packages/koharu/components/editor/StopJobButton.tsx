'use client'

import { LoaderCircle, Square } from 'lucide-react'
import { useTranslation } from 'react-i18next'

import { stopJob } from '@/lib/backend'
import { useKoharuStore } from '@/lib/store'
import { Button } from '@koharu/ui/components/button'

export function StopJobButton({
  id,
  label = false,
  className,
}: {
  id: string
  label?: boolean
  className?: string
}) {
  const { t } = useTranslation()
  const stopping = useKoharuStore((state) => Boolean(state.stoppingJobs[id]))
  const text = t(stopping ? 'activity.stopping' : 'activity.stop')
  return (
    <Button
      type='button'
      size={label ? 'sm' : 'icon-xs'}
      variant={label ? 'default' : 'ghost'}
      className={className}
      disabled={stopping}
      aria-label={!stopping && label ? t('inference.stopProcessing') : text}
      title={stopping ? t('activity.stoppingHint') : text}
      onClick={() => void stopJob(id).catch(() => undefined)}
    >
      {stopping ? (
        <LoaderCircle className='size-3 animate-spin motion-reduce:animate-none' />
      ) : (
        <Square className='size-3 fill-current' />
      )}
      {label && <span>{text}</span>}
    </Button>
  )
}
