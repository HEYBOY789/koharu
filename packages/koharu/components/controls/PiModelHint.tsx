'use client'

import { useTranslation } from 'react-i18next'

export function PiModelHint() {
  const { t } = useTranslation()
  return (
    <div role='note' className='mt-2 min-w-0 text-[10px] leading-4 text-muted-foreground'>
      <p>{t('agent.piModelHint')}</p>
      <code className='mt-1 block break-all text-foreground'>
        local-proxy/ag/gemini-3.8-flash-high
      </code>
    </div>
  )
}
