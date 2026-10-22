'use client'

import { BookOpen, Bot, SlidersHorizontal } from 'lucide-react'
import { useEffect } from 'react'
import { useTranslation } from 'react-i18next'

import { AgentPanel } from '@/components/editor/AgentPanel'
import { ChapterPanel } from '@/components/editor/ChapterPanel'
import { Inspector } from '@/components/editor/Inspector'
import { usePage } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { Button } from '@koharu/ui/components/button'

export function RightSidebar() {
  const { t } = useTranslation()
  const panel = useKoharuStore((state) => state.inspectorPanel)
  const setPanel = useKoharuStore((state) => state.setInspectorPanel)
  const chapter = usePage().data?.chapter

  useEffect(() => {
    if (!chapter && panel === 'chapter') setPanel('properties')
  }, [chapter, panel, setPanel])

  return (
    <aside className='flex h-full min-h-0 flex-col bg-[var(--surface-panel)]'>
      <div className='flex h-10 shrink-0 items-center gap-1 border-b border-border/80 px-2.5'>
        <Button
          variant={panel === 'properties' ? 'secondary' : 'ghost'}
          size='sm'
          className='h-7 flex-1 text-[10px]'
          onClick={() => setPanel('properties')}
        >
          <SlidersHorizontal className='size-3' /> {t('agent.properties')}
        </Button>
        <Button
          variant={panel === 'agent' ? 'secondary' : 'ghost'}
          size='sm'
          className='h-7 flex-1 px-1 text-[10px]'
          onClick={() => setPanel('agent')}
        >
          <Bot className='size-3' /> {t('agent.title')}
        </Button>
        {chapter ? (
          <Button
            variant={panel === 'chapter' ? 'secondary' : 'ghost'}
            size='sm'
            className='h-7 flex-1 px-1 text-[10px]'
            onClick={() => setPanel('chapter')}
          >
            <BookOpen className='size-3' /> {t('chapter.title')}
          </Button>
        ) : null}
      </div>
      <div className={panel === 'agent' ? 'flex min-h-0 flex-1 flex-col' : 'hidden'}>
        <AgentPanel />
      </div>
      <div className={panel === 'chapter' ? 'flex min-h-0 flex-1 flex-col' : 'hidden'}>
        <ChapterPanel />
      </div>
      <div className={panel === 'properties' ? 'min-h-0 flex-1' : 'hidden'}>
        <Inspector />
      </div>
    </aside>
  )
}
