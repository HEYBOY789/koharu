'use client'

import { BookOpen, Folder } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { MangaDexSearchPage } from '@/components/app/MangaDexSearchPage'
import { TitleBar } from '@/components/app/TitleBar'
import { ActivityCenter } from '@/components/editor/ActivityCenter'
import { Editor } from '@/components/editor/Editor'
import { SettingsPage } from '@/components/preferences/SettingsPage'
import { StartView } from '@/components/start/StartView'
import { useProject } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { Button } from '@koharu/ui/components/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@koharu/ui/components/tabs'
import { cn } from '@koharu/ui/lib/utils'

export function KoharuApp() {
  const { t } = useTranslation()
  const [libraryOpen, setLibraryOpen] = useState(false)
  const project = useProject().data
  const selectedPages = useKoharuStore((state) => state.selectedPages)
  const selectPages = useKoharuStore((state) => state.selectPages)
  const selectLayers = useKoharuStore((state) => state.selectLayers)
  const projectLoaded = project !== undefined
  const settingsOpen = useKoharuStore((state) => state.settingsOpen)
  const workspaceTab = useKoharuStore((state) => state.workspaceTab)
  const setWorkspaceTab = useKoharuStore((state) => state.setWorkspaceTab)
  const activePage = project?.active_page
  const editorOpen =
    project !== undefined &&
    project !== null &&
    !settingsOpen &&
    workspaceTab === 'projects' &&
    !libraryOpen

  useEffect(() => setLibraryOpen(false), [project?.name])

  useEffect(() => {
    if (!projectLoaded || selectedPages.length > 0) return
    selectPages(activePage ? [activePage] : [])
    selectLayers([])
  }, [activePage, projectLoaded, selectLayers, selectPages, selectedPages.length])

  return (
    <div
      className={cn(
        'fixed inset-0 flex flex-col overflow-hidden text-foreground',
        editorOpen ? 'bg-transparent' : 'bg-[var(--surface-titlebar)]',
      )}
    >
      <TitleBar />
      <Tabs
        value={workspaceTab}
        onValueChange={(value) => {
          if (value === 'projects' || value === 'mangadex') setWorkspaceTab(value)
        }}
        className={cn('min-h-0 flex-1 gap-0', settingsOpen && 'hidden')}
      >
        <div className='flex min-w-0 shrink-0 items-center gap-2'>
          <TabsList
            variant='line'
            aria-label={t('mangadex.navigation')}
            className='mx-4 my-1 shrink-0'
          >
            <TabsTrigger value='projects' className='px-3'>
              <Folder />
              {t('start.title')}
            </TabsTrigger>
            <TabsTrigger value='mangadex' className='px-3'>
              <BookOpen />
              {t('mangadex.tab')}
            </TabsTrigger>
          </TabsList>
          {project && (
            <Button
              variant='ghost'
              size='sm'
              className='max-w-64 min-w-0 text-xs'
              onClick={() => {
                setWorkspaceTab('projects')
                setLibraryOpen((current) => !current)
              }}
            >
              {libraryOpen ? <BookOpen /> : <Folder />}
              <span className='truncate'>
                {libraryOpen
                  ? t('start.returnToProject', { name: project.name })
                  : t('start.library')}
              </span>
            </Button>
          )}
        </div>
        <TabsContent
          value='projects'
          keepMounted
          className='flex min-h-0 flex-col data-[hidden]:hidden'
        >
          {project === undefined ? (
            <main className='grid min-h-0 flex-1 place-items-center bg-[var(--surface-canvas)]'>
              <div className='flex items-center gap-3 text-[12px] text-muted-foreground'>
                <span className='size-2 rounded-full bg-primary' />
                {t('startup.opening')}
              </div>
            </main>
          ) : (
            <>
              {(project === null || libraryOpen) && (
                <StartView onOpenProject={() => setLibraryOpen(false)} />
              )}
              {project && (
                <div className={cn('flex min-h-0 flex-1 flex-col', libraryOpen && 'hidden')}>
                  <Editor key={project.name} />
                </div>
              )}
            </>
          )}
        </TabsContent>
        <TabsContent value='mangadex' keepMounted className='min-h-0 data-[hidden]:hidden'>
          <MangaDexSearchPage active={!settingsOpen && workspaceTab === 'mangadex'} />
        </TabsContent>
      </Tabs>
      {settingsOpen && <SettingsPage />}
      <ActivityCenter />
    </div>
  )
}
