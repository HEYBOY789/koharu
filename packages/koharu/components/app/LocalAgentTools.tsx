'use client'

import { useTranslation } from 'react-i18next'

import { call } from '@/lib/backend'
import {
  commands,
  type PublicationConfig,
  type PublicationReadiness,
} from '@koharu/bridge/protocol'
import { Button } from '@koharu/ui/components/button'
import { Textarea } from '@koharu/ui/components/textarea'

export function LocalAgentTools({
  config,
  readiness,
  disabled,
  checking,
  update,
  recheck,
  onError,
}: {
  config: PublicationConfig
  readiness?: PublicationReadiness
  disabled: boolean
  checking: boolean
  update: (value: Partial<PublicationConfig>) => void
  recheck: () => void
  onError: (message: string) => void
}) {
  const { t } = useTranslation()
  async function pick(kind: 'script' | 'node' | 'binary') {
    try {
      const selected = await call(commands.pickPublicationFile, kind)
      if (selected) update({ [kind]: selected })
    } catch (cause) {
      onError(String(cause))
    }
  }
  return (
    <details>
      <summary className='cursor-pointer text-sm'>{t('publication.tools')}</summary>
      <fieldset disabled={disabled} className='mt-3 space-y-3'>
        <Button
          type='button'
          variant='outline'
          disabled={checking}
          onClick={() => {
            update({ node: 'node', binary: null })
            recheck()
          }}
        >
          {t('publication.detect')}
        </Button>
        {(['script', 'node', 'binary'] as const).map((key) => (
          <div key={key} className='space-y-1 text-xs'>
            <label htmlFor={`local-agent-${key}`}>{t(`publication.${key}`)}</label>
            <div className='flex gap-2'>
              <Textarea
                id={`local-agent-${key}`}
                rows={2}
                className='resize-y font-mono break-all'
                value={readiness?.[key] || config[key] || ''}
                onChange={(event) =>
                  update({
                    [key]: event.target.value || (key === 'binary' ? null : ''),
                  })
                }
              />
              <Button
                type='button'
                variant='outline'
                onClick={() => void pick(key)}
                aria-label={t('publication.chooseFile', {
                  tool: t(`publication.${key}`),
                })}
              >
                {t('publication.choose')}
              </Button>
            </div>
          </div>
        ))}
      </fieldset>
    </details>
  )
}
