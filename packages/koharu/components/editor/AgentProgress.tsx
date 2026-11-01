'use client'

import type { TFunction } from 'i18next'
import { useTranslation } from 'react-i18next'

import type { AgentRun } from '@koharu/bridge/protocol'

export function agentProgressLabel(t: TFunction, run: AgentRun): string {
  if (run.state !== 'running')
    return t(`activity.${run.state === 'failed' ? 'processingFailed' : run.state}`)
  if (run.phase === 'tool' && run.tool) {
    return [t(`agent.tools.${run.tool}`, { defaultValue: t('agent.applying') }), run.page]
      .filter(Boolean)
      .join(' · ')
  }
  if (run.phase === 'pipeline')
    return [run.stage ? t(`phase.${run.stage}`) : t('activity.processing'), run.page]
      .filter(Boolean)
      .join(' · ')
  if (run.phase === 'thinking') return t('agent.thinking')
  return t(`agent.progress.${run.phase}`)
}

export function AgentProgress({ run }: { run: AgentRun }) {
  const { t } = useTranslation()
  return (
    <div
      role='status'
      className='grid min-h-10 min-w-0 content-center gap-0.5 text-[11px] leading-5 text-muted-foreground'
    >
      <span className='break-words'>{agentProgressLabel(t, run)}</span>
      {run.state === 'running' && run.error && (
        <div role='note' className='grid gap-1 break-words text-amber-700 dark:text-amber-400'>
          <span>{t('agent.progress.toolRejected')}</span>
          <span className='text-muted-foreground'>{run.error}</span>
        </div>
      )}
      {run.changes > 0 && (
        <span className='text-[10px]'>{t('agent.progress.changes', { count: run.changes })}</span>
      )}
    </div>
  )
}
