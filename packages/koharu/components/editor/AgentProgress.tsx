'use client'

import type { TFunction } from 'i18next'
import { useTranslation } from 'react-i18next'

import type { AgentRun } from '@koharu/bridge/protocol'

export function agentProgressLabel(t: TFunction, run: AgentRun, includePage = true): string {
  const phase = agentPhaseLabel(t, run)
  return includePage
    ? [
        phase,
        agentPageLabel(t, run),
        run.failures.length
          ? t('agent.progress.pendingFailures', { count: run.failures.length })
          : null,
        run.readability ? t('agent.progress.readability', run.readability) : null,
      ]
        .filter(Boolean)
        .join(' · ')
    : phase
}

function agentPageLabel(t: TFunction, run: AgentRun): string | null {
  if (run.state === 'running' && (run.phase === 'tool' || run.phase === 'pipeline') && run.page)
    return t('agent.progress.targetPage', { page: run.page })
  const page = run.last_page
  return page ? t('agent.progress.lastPage', { page }) : null
}

function agentPhaseLabel(t: TFunction, run: AgentRun): string {
  if (run.state !== 'running')
    return t(`activity.${run.state === 'failed' ? 'processingFailed' : run.state}`)
  if (run.phase === 'tool' && run.tool) {
    return t(`agent.tools.${run.tool}`, { defaultValue: t('agent.applying') })
  }
  if (run.phase === 'pipeline')
    return run.stage ? t(`phase.${run.stage}`) : t('activity.processing')
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
      <span className='break-words'>{agentProgressLabel(t, run, false)}</span>
      {agentPageLabel(t, run) && (
        <span className='font-medium break-words text-foreground'>{agentPageLabel(t, run)}</span>
      )}
      {run.readability && (
        <div className='grid min-w-0 gap-0.5 break-words'>
          <span>{t('agent.progress.readability', run.readability)}</span>
          {run.readability.needs_review > 0 && (
            <span className='text-amber-700 dark:text-amber-400'>
              {t('agent.progress.readabilityNeedsReview', { count: run.readability.needs_review })}
            </span>
          )}
        </div>
      )}
      {run.failures.length > 0 && (
        <details
          open
          role='note'
          className='min-w-0 rounded-lg border border-amber-700/30 p-2 text-amber-700 dark:text-amber-400'
        >
          <summary className='cursor-pointer font-medium'>
            {t('agent.progress.pendingFailures', { count: run.failures.length })}
          </summary>
          <ul className='mt-2 grid gap-3'>
            {run.failures.map((failure) => (
              <li key={failure.id} className='grid min-w-0 gap-1 break-words'>
                <span className='font-medium'>
                  {failure.page ?? t('agent.progress.projectTool')}
                </span>
                <span>{t(`agent.tools.${failure.tool}`, { defaultValue: failure.tool })}</span>
                <span className='text-muted-foreground'>{failure.error}</span>
              </li>
            ))}
          </ul>
        </details>
      )}
      {run.state === 'running' && run.error && run.failures.length === 0 && (
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
