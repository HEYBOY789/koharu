'use client'

import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { LocalAgentTools } from '@/components/app/LocalAgentTools'
import { useProject } from '@/lib/queries'
import {
  commands,
  type Labels,
  type PublicationConfig,
  type PublicationSettings,
} from '@koharu/ipc/protocol'
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
import { NativeSelect, NativeSelectOption } from '@koharu/ui/components/native-select'

export function PublicationDialog({ labels, close }: { labels: Labels; close: () => void }) {
  const { t } = useTranslation()
  const project = useProject().data?.id
  const saved = useQuery({
    queryKey: ['publication', project],
    queryFn: commands.getPublicationSettings,
    staleTime: 0,
  })
  const [draft, setDraft] = useState<PublicationSettings | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [location, setLocation] = useState('')
  const value = draft ?? saved.data
  const readiness = useQuery({
    queryKey: ['publication-tools', value?.config],
    queryFn: () => commands.inspectPublication(value!.config),
    enabled: Boolean(value),
  })
  const configure = (patch: Partial<PublicationConfig>) => {
    if (value) setDraft({ ...value, config: { ...value.config, ...patch } })
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) close()
      }}
    >
      <DialogContent className='max-h-[85vh] overflow-auto'>
        <DialogHeader>
          <DialogTitle>{t('publication.prepareBundle')}</DialogTitle>
          <DialogDescription>{t('publication.localBundle')}</DialogDescription>
        </DialogHeader>
        {value && (
          <form
            className='grid gap-3'
            onSubmit={(event) => {
              event.preventDefault()
              setBusy(true)
              setError('')
              setLocation('')
              void commands
                .preparePublication(value, labels)
                .then((path) => {
                  if (path) setLocation(path)
                })
                .catch((cause) => setError(String(cause)))
                .finally(() => setBusy(false))
            }}
          >
            <fieldset disabled={busy} className='grid gap-2'>
              {(['title', 'slug', 'author'] as const).map((field) => (
                <label key={field} className='grid gap-1 text-xs'>
                  {t(`publication.metadata.${field}`)}
                  <Input
                    value={value.metadata[field]}
                    onChange={(event) =>
                      setDraft({
                        ...value,
                        metadata: { ...value.metadata, [field]: event.currentTarget.value },
                      })
                    }
                  />
                </label>
              ))}
              <label className='grid gap-1 text-xs'>
                {t('publication.metadata.status')}
                <NativeSelect
                  value={value.metadata.status}
                  onChange={(event) =>
                    setDraft({
                      ...value,
                      metadata: { ...value.metadata, status: event.currentTarget.value },
                    })
                  }
                >
                  {(['ongoing', 'completed', 'hiatus'] as const).map((status) => (
                    <NativeSelectOption key={status} value={status}>
                      {t(`mangadex.${status}`)}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </label>
              <label className='grid gap-1 text-xs'>
                {t('publication.catalog')}
                <Input
                  value={value.config.catalog_url ?? ''}
                  onChange={(event) => configure({ catalog_url: event.currentTarget.value })}
                />
              </label>
              <label className='flex items-center gap-2 text-xs'>
                <Checkbox
                  checked={value.config.offline ?? false}
                  onCheckedChange={(checked) => configure({ offline: checked === true })}
                />
                {t('publication.offline')}
              </label>
              <LocalAgentTools
                config={value.config}
                readiness={readiness.data}
                disabled={busy}
                checking={readiness.isFetching}
                update={configure}
                recheck={() => void readiness.refetch()}
                onError={setError}
              />
              <Button
                type='submit'
                disabled={
                  !value.metadata.title.trim() ||
                  !value.metadata.slug.trim() ||
                  !value.config.catalog_url?.trim()
                }
              >
                {busy ? t('common.loading') : t('publication.prepareBundle')}
              </Button>
            </fieldset>
            {error && (
              <p role='alert' className='text-xs break-words text-destructive'>
                {error}
              </p>
            )}
            {location && (
              <p role='status' className='text-xs break-all'>
                {location}
              </p>
            )}
          </form>
        )}
        {saved.error && (
          <p role='alert' className='text-xs text-destructive'>
            {String(saved.error)}
          </p>
        )}
      </DialogContent>
    </Dialog>
  )
}
