'use client'

import { Bell, BookOpen, Folder } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { FollowingPage } from '@/components/app/FollowingPage'
import { MangaDexSearchPage } from '@/components/app/MangaDexSearchPage'
import { StartView } from '@/components/start/StartView'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@koharu/ui/components/tabs'

export function ProjectLibrary() {
  const { t } = useTranslation()
  const [tab, setTab] = useState('projects')
  return (
    <Tabs value={tab} onValueChange={setTab} className='min-h-0 flex-1 gap-0'>
      <TabsList variant='line' className='justify-start border-b px-3'>
        <TabsTrigger value='projects'>
          <Folder />
          {t('start.title')}
        </TabsTrigger>
        <TabsTrigger value='mangadex'>
          <BookOpen />
          MangaDex
        </TabsTrigger>
        <TabsTrigger value='following'>
          <Bell />
          {t('following.title')}
        </TabsTrigger>
      </TabsList>
      <TabsContent value='projects' className='flex min-h-0 flex-1'>
        <StartView />
      </TabsContent>
      <TabsContent value='mangadex' keepMounted className='min-h-0 flex-1 overflow-y-auto'>
        <MangaDexSearchPage active={tab === 'mangadex'} />
      </TabsContent>
      <TabsContent value='following' keepMounted className='min-h-0 flex-1 overflow-y-auto'>
        <FollowingPage active={tab === 'following'} />
      </TabsContent>
    </Tabs>
  )
}
