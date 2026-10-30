'use client'

import { useMutationState } from '@tanstack/react-query'
import {
  Check,
  ChevronUp,
  CircleAlert,
  Download,
  FolderOpen,
  LoaderCircle,
  Square,
  X,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import { usePages, useProject } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands, type Download as DownloadState, type Job } from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'

import { AgentProgress, agentProgressLabel } from './AgentProgress'

export function ActivityCenter() {
  const { t } = useTranslation()
  const [expanded, setExpanded] = useState(false)
  const project = useProject().data
  const jobs = useKoharuStore((state) => state.jobs)
  const agent = useKoharuStore((state) => state.agent.run)
  const downloads = useKoharuStore((state) => state.downloads)
  const activities = useMutationState({
    filters: { status: 'pending' },
    select: (mutation) => ({
      id: mutation.mutationId,
      label: mutation.options.meta?.activity,
    }),
  }).filter(
    (activity): activity is { id: number; label: string } => typeof activity.label === 'string',
  )
  const visibleJobs = Object.values(jobs).reverse()
  const runningDownloads = Object.values(downloads).filter(
    (download) => download.state === 'running',
  )
  const failedDownloads = Object.values(downloads).filter((download) => download.state === 'failed')
  const visibleDownloads = [...runningDownloads, ...failedDownloads]
  const empty =
    activities.length === 0 &&
    visibleJobs.length === 0 &&
    visibleDownloads.length === 0 &&
    agent === null
  useEffect(() => {
    if (empty) setExpanded(false)
  }, [empty])
  if (empty) return null

  const primary =
    visibleJobs.find((job) => job.state === 'running') ??
    visibleJobs.find((job) => job.state === 'queued') ??
    visibleJobs.find((job) => job.state === 'failed') ??
    visibleJobs[0]
  const agentPrimary = agent && (agent.state === 'running' || !primary)
  const percent = primary ? progress(primary.completed, primary.total) : null
  const errors =
    visibleJobs.filter((job) => job.state === 'failed').length +
    failedDownloads.length +
    Number(agent?.state === 'failed')
  const label = agentPrimary
    ? `${agent.project} · ${t('agent.title')} · ${agentProgressLabel(t, agent)}`
    : primary
      ? [
          primary.project,
          primary.state === 'running'
            ? (primary.chapter_title ??
              (primary.stage ? t(`phase.${primary.stage}`) : t('activity.processing')))
            : t(
                primary.state === 'failed'
                  ? 'activity.processingFailed'
                  : `activity.${primary.state}`,
              ),
          primary.state === 'running' && percent !== null ? `${percent}%` : null,
        ]
          .filter(Boolean)
          .join(' · ')
      : activities[0]
        ? t(activities[0].label)
        : t('activity.downloadingFiles', { count: visibleDownloads.length })

  return (
    <section
      aria-label={t('activity.title')}
      className='shrink-0 border-t border-border/60 bg-[var(--surface-toolbar)]'
    >
      <div className='flex h-8 min-w-0 items-center gap-1 px-2'>
        <Button
          variant='ghost'
          size='sm'
          className='h-7 min-w-0 flex-1 justify-start text-[11px]'
          aria-label={t('activity.details')}
          aria-expanded={expanded}
          aria-controls='activity-details'
          onClick={() => setExpanded((value) => !value)}
          title={label}
        >
          <ChevronUp className={`size-3 shrink-0 ${expanded ? 'rotate-180' : ''}`} />
          <span className='shrink-0 text-muted-foreground'>{t('activity.title')}</span>
          <span className='truncate'>{label}</span>
          {errors > 0 ? (
            <span
              className='ml-auto flex shrink-0 items-center gap-1 text-destructive'
              aria-label={t('activity.attention', { count: errors })}
            >
              <CircleAlert className='size-3' />
              {errors}
            </span>
          ) : null}
        </Button>
        {agentPrimary ? (
          <>
            {agent.project !== project?.name && <OpenProject name={agent.project} agent />}
            {agent.state === 'running' && (
              <Button
                size='icon-xs'
                variant='ghost'
                aria-label={t('activity.stop')}
                onClick={() => void call(commands.cancelAgent, agent.id).catch(() => undefined)}
              >
                <Square className='size-2.5 fill-current' />
              </Button>
            )}
          </>
        ) : (
          <>
            {primary?.project &&
            primary.project !== project?.name &&
            primary.state !== 'queued' &&
            !(primary.state === 'running' && primary.phase === 'import') ? (
              <OpenProject name={primary.project} />
            ) : null}
            {primary?.state === 'running' || primary?.state === 'queued' ? (
              <Button
                size='icon-xs'
                variant='ghost'
                aria-label={t('activity.stop')}
                onClick={() => void call(commands.stopJob, primary.id).catch(() => undefined)}
              >
                <Square className='size-2.5 fill-current' />
              </Button>
            ) : null}{' '}
          </>
        )}
      </div>
      {expanded ? (
        <div
          id='activity-details'
          role='region'
          aria-label={t('activity.details')}
          className='max-h-[min(40vh,20rem)] overflow-y-auto border-t border-border/60 bg-popover'
        >
          {activities.map((activity) => (
            <div
              key={activity.id}
              role='status'
              className='grid grid-cols-[1rem_minmax(0,1fr)] items-center gap-x-2.5 border-b p-3 last:border-b-0'
            >
              <LoaderCircle
                className='size-3.5 animate-spin justify-self-center text-primary motion-reduce:animate-none'
                aria-hidden='true'
              />
              <span className='truncate text-[12px] font-medium'>{t(activity.label)}</span>
            </div>
          ))}
          {runningDownloads.length > 1 ? (
            <DownloadGroup downloads={runningDownloads} />
          ) : (
            runningDownloads.map((download) => (
              <DownloadItem key={download.id} download={download} />
            ))
          )}
          {failedDownloads.map((download) => (
            <DownloadItem key={download.id} download={download} />
          ))}
          {agent && (
            <div className='border-b p-3 text-[11px] last:border-b-0'>
              <div className='flex min-w-0 items-center gap-2'>
                <span className='min-w-0 flex-1 truncate font-medium'>
                  {agent.project} · {t('agent.title')}
                </span>
                {agent.project !== project?.name && <OpenProject name={agent.project} agent />}
                {agent.state === 'running' ? (
                  <Button
                    size='icon-xs'
                    variant='ghost'
                    aria-label={t('activity.stop')}
                    onClick={() => void call(commands.cancelAgent, agent.id).catch(() => undefined)}
                  >
                    <Square className='size-2.5 fill-current' />
                  </Button>
                ) : (
                  <Button
                    size='icon-xs'
                    variant='ghost'
                    aria-label={t('activity.dismiss')}
                    onClick={() => void call(commands.dismissAgentActivity)}
                  >
                    <X className='size-3' />
                  </Button>
                )}
              </div>
              <AgentProgress run={agent} />
              {agent.state !== 'running' && agent.error && (
                <p className='break-words text-destructive'>{agent.error}</p>
              )}
            </div>
          )}
          {visibleJobs.map((job) => (
            <JobItem key={job.id} job={job} />
          ))}
        </div>
      ) : null}
    </section>
  )
}

function OpenProject({ name, agent = false }: { name: string; agent?: boolean }) {
  const { t } = useTranslation()
  const setWorkspaceTab = useKoharuStore((state) => state.setWorkspaceTab)
  return (
    <Button
      size='icon-xs'
      variant='ghost'
      aria-label={t('activity.openProject', { name })}
      title={t('activity.openProject', { name })}
      onClick={() =>
        void call(commands.openProject, name)
          .then(() => {
            setWorkspaceTab('projects')
            if (agent) useKoharuStore.getState().setInspectorPanel('agent')
          })
          .catch(() => undefined)
      }
    >
      <FolderOpen className='size-3' />
    </Button>
  )
}

function DownloadGroup({ downloads }: { downloads: DownloadState[] }) {
  const { t } = useTranslation()
  const hasKnownTotal = downloads.every((download) => download.total > 0)
  const percent = hasKnownTotal
    ? progress(
        downloads.reduce((sum, download) => sum + download.completed, 0),
        downloads.reduce((sum, download) => sum + download.total, 0),
      )
    : null

  return (
    <div className='border-b p-3 last:border-b-0'>
      <div className='grid grid-cols-[1rem_minmax(0,1fr)_2.25rem_1.5rem] items-start gap-x-2.5'>
        <Download className='mt-0.5 size-3.5 justify-self-center text-primary' />
        <span className='truncate text-[11px]'>
          {t('activity.downloadingFiles', { count: downloads.length })}
        </span>
        <span className='text-right text-[10px] tabular-nums'>
          {percent !== null ? `${percent}%` : null}
        </span>
        <span aria-hidden='true' />
        <div className='col-start-2 col-end-4'>
          <Progress value={percent} />
        </div>
      </div>
    </div>
  )
}

function JobItem({ job }: { job: Job }) {
  const { t } = useTranslation()
  const dismiss = useKoharuStore((state) => state.dismissJob)
  // The pipeline reports which page it is on; the rail has already loaded the
  // labels, so this resolves from cache rather than fetching.
  const active = useProject().data
  const sameProject = job.project !== null && job.project === active?.name
  const pagesQuery = usePages(job.page !== null && sameProject)
  const pages = sameProject ? pagesQuery.data : undefined
  // Numbered by position in the rail, so the row says where in the run this
  // is as well as which file: a scan filename alone rarely tells you.
  const pageIndex = job.page ? (pages?.findIndex((page) => page.id === job.page) ?? -1) : -1
  const page = pageIndex >= 0 ? pages?.[pageIndex] : undefined
  const pageLabel = page
    ? t('activity.pageLabel', { number: pageIndex + 1, label: page.label })
    : undefined
  if (job.state === 'failed') {
    return (
      <div className='border-b last:border-b-0'>
        <JobProjectHeader job={job} />
        <Failure
          message={job.error || t('activity.processingFailed')}
          onDismiss={() => dismiss(job.id)}
        />
      </div>
    )
  }
  if (job.state === 'queued') {
    return (
      <div className='flex items-center gap-2 border-b p-3 text-xs'>
        <span className='min-w-0 flex-1 break-words'>
          {job.project || job.chapter_title} · {t('activity.queued')}
        </span>
        <Button
          size='icon-xs'
          variant='ghost'
          aria-label={t('activity.stop')}
          onClick={() => void call(commands.stopJob, job.id).catch(() => undefined)}
        >
          <Square className='size-2.5 fill-current' />
        </Button>
      </div>
    )
  }
  if (job.state !== 'running') {
    return (
      <div className='border-b last:border-b-0'>
        <JobProjectHeader job={job} />
        <div className='flex items-center gap-2 p-3 text-[11px]'>
          <Check className='size-3 text-muted-foreground' />
          <span className='flex-1'>{t(`activity.${job.state}`)}</span>
          <Button
            size='icon-xs'
            variant='ghost'
            aria-label={t('activity.dismiss')}
            onClick={() => dismiss(job.id)}
          >
            <X />
          </Button>
        </div>
      </div>
    )
  }
  const percent = progress(job.completed, job.total)
  return (
    <div className='border-b last:border-b-0'>
      <JobProjectHeader job={job} />
      <div className='p-3'>
        <div className='grid grid-cols-[1rem_minmax(0,1fr)_2.25rem_1.5rem] items-start gap-x-2.5'>
          <span className='mt-1.5 size-1.5 justify-self-center rounded-full bg-primary' />
          <div className='min-w-0'>
            <span className='block truncate text-[12px] font-medium'>
              {job.chapter_title && job.phase
                ? t(
                    job.phase === 'import'
                      ? 'sourceImport.downloading'
                      : job.phase === 'summarize'
                        ? 'activity.summarizingChapter'
                        : 'activity.translatingChapter',
                    { chapter: job.chapter_title },
                  )
                : job.stage
                  ? t(`phase.${job.stage}`, { defaultValue: job.stage })
                  : t('activity.processing')}
            </span>
          </div>
          <span className='pt-0.5 text-right text-[10px] tabular-nums'>
            {percent !== null ? `${percent}%` : null}
          </span>
          <Button
            size='icon-xs'
            variant='ghost'
            className='-mt-1'
            aria-label={t('activity.stop')}
            onClick={() => void call(commands.stopJob, job.id).catch(() => undefined)}
          >
            <Square className='size-2.5 fill-current' />
          </Button>
          {pageLabel ? (
            <p className='col-start-2 col-end-4 mt-0.5 truncate text-[10px] text-muted-foreground'>
              {pageLabel}
            </p>
          ) : null}
          {job.detail ? (
            <p className='col-start-2 col-end-4 line-clamp-2 text-[9px] leading-3.5 break-words text-muted-foreground'>
              {job.detail}
            </p>
          ) : null}
          {job.retry ? (
            <p
              role='status'
              className='col-start-2 col-end-4 mt-1 text-[10px] leading-4 break-words text-muted-foreground'
            >
              {t('activity.retrying', {
                attempt: job.retry.attempt,
                max: job.retry.max_attempts,
                seconds: job.retry.delay_seconds,
              })}
            </p>
          ) : null}
          {job.model ? (
            <p className='col-start-2 col-end-4 truncate text-[10px] text-muted-foreground'>
              {job.model}
            </p>
          ) : null}
          <div className='col-start-2 col-end-4'>
            <Progress value={percent} />
          </div>
        </div>
      </div>
    </div>
  )
}

function JobProjectHeader({ job }: { job: Job }) {
  const active = useProject().data
  return job.project ? (
    <div className='flex min-w-0 items-center gap-2 px-3 pt-2 text-[11px] font-semibold'>
      <span className='truncate' title={job.project}>
        {job.project}
      </span>
      {job.project !== active?.name &&
      job.state !== 'queued' &&
      !(job.state === 'running' && job.phase === 'import') ? (
        <OpenProject name={job.project} />
      ) : null}
    </div>
  ) : null
}

function DownloadItem({ download }: { download: DownloadState }) {
  const { t } = useTranslation()
  const dismiss = useKoharuStore((state) => state.dismissDownload)
  if (download.state === 'failed') {
    return (
      <Failure
        message={download.error || t('activity.downloadFailed')}
        onDismiss={() => dismiss(download.id)}
      />
    )
  }
  const percent = progress(download.completed, download.total)
  return (
    <div className='border-b p-3 last:border-b-0'>
      <div className='grid grid-cols-[1rem_minmax(0,1fr)_2.25rem_1.5rem] items-start gap-x-2.5'>
        <Download className='mt-0.5 size-3.5 justify-self-center text-primary' />
        <span className='truncate text-[11px]'>{download.name || t('activity.modelDownload')}</span>
        <span className='text-right text-[10px] tabular-nums'>
          {percent !== null ? `${percent}%` : null}
        </span>
        <span aria-hidden='true' />
        <div className='col-start-2 col-end-4'>
          <Progress value={percent} />
        </div>
      </div>
    </div>
  )
}

function Failure({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  const { t } = useTranslation()
  return (
    <div className='grid grid-cols-[1rem_minmax(0,1fr)_2.25rem_1.5rem] items-start gap-x-2.5 border-b p-3 text-[11px] last:border-b-0'>
      <CircleAlert className='mt-0.5 size-3.5 justify-self-center text-destructive' />
      <span className='col-start-2 col-end-4 max-h-40 min-w-0 overflow-y-auto [overflow-wrap:anywhere] whitespace-pre-wrap text-destructive'>
        {message}
      </span>
      <Button
        size='icon-xs'
        variant='ghost'
        className='-mt-1'
        aria-label={t('activity.dismiss')}
        onClick={onDismiss}
      >
        <X />
      </Button>
    </div>
  )
}

function Progress({ value }: { value: number | null }) {
  return (
    <div
      role='progressbar'
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value ?? undefined}
      className='mt-2 h-1 overflow-hidden rounded-full bg-muted'
    >
      <div
        className={`h-full rounded-full bg-primary ${value === null ? 'w-1/2' : ''}`}
        style={value === null ? undefined : { width: `${value}%` }}
      />
    </div>
  )
}

function progress(completed: number, total: number): number | null {
  return total > 0 ? Math.min(100, Math.round((completed / total) * 100)) : null
}
