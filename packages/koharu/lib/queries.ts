'use client'

import {
  QueryClient,
  type QueryKey,
  queryOptions,
  useIsMutating,
  useMutation,
  useQuery,
} from '@tanstack/react-query'

import { commands, type FontFamily } from '@koharu/bridge/protocol'

import { call } from './backend'
import { useKoharuStore } from './store'

export const projectKey = ['project'] as const
export const followingKey = ['following'] as const

export function useFollowing(enabled = true) {
  return useQuery(
    { queryKey: followingKey, queryFn: () => call(commands.getFollowing), enabled },
    queryClient,
  )
}
export const pagesKey = ['pages'] as const
export const pageKey = ['page'] as const
export const chaptersKey = ['chapters'] as const
export const chapterKey = (chapter: string) => ['chapter', chapter] as const
export const preparedPageKey = (page: string) => ['prepared-page', page] as const
export const fontsKey = ['fonts'] as const
export const summarizerConfigKey = ['summarizer-config'] as const

const projectQuery = queryOptions({
  queryKey: projectKey,
  queryFn: () => call(commands.getProject),
})

const pagesQuery = queryOptions({
  queryKey: pagesKey,
  queryFn: () => call(commands.getPages),
})

const pageQuery = queryOptions({
  queryKey: pageKey,
  queryFn: () => call(commands.getPage),
})

const fontsQuery = queryOptions({
  queryKey: fontsKey,
  queryFn: () => call(commands.getFonts),
})

const chaptersQuery = queryOptions({
  queryKey: chaptersKey,
  queryFn: () => call(commands.getChapters),
})

const summarizerConfigQuery = queryOptions({
  queryKey: summarizerConfigKey,
  queryFn: () => call(commands.getSummarizerConfig),
})

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: Number.POSITIVE_INFINITY,
      retry: false,
      refetchOnReconnect: false,
      refetchOnWindowFocus: false,
    },
  },
})

export function useProject(enabled = true) {
  return useQuery({ ...projectQuery, enabled })
}

export function useProjectBusy() {
  const name = useProject().data?.name
  return useKoharuStore((state) =>
    Object.values(state.jobs).some(
      (job) => job.state === 'running' && (job.project === null || job.project === name),
    ),
  )
}

export function usePages(enabled = true) {
  return useQuery({ ...pagesQuery, enabled })
}

export function usePage(enabled = true) {
  return useQuery({ ...pageQuery, enabled })
}

export function useFonts(enabled = true) {
  return useQuery({ ...fontsQuery, enabled })
}

export function useChapters(enabled = true) {
  return useQuery({ ...chaptersQuery, enabled })
}

export function useChapter(chapter: string | undefined, enabled = true) {
  return useQuery({
    queryKey: chapterKey(chapter ?? ''),
    queryFn: () => call(commands.getChapter, chapter as string),
    enabled: enabled && Boolean(chapter),
  })
}

export function useSummarizerConfig(enabled = true) {
  return useQuery({ ...summarizerConfigQuery, enabled })
}

export function useFontPreview(font: FontFamily | undefined, enabled = true) {
  return useQuery({
    queryKey: ['font-preview', font?.name],
    queryFn: async () => {
      if (!font) return null
      try {
        return new Uint8Array(await commands.getFontPreview(font.name))
      } catch {
        return null
      }
    },
    enabled: enabled && font !== undefined,
    gcTime: 5 * 60 * 1000,
  })
}

export function useCommand<Args extends unknown[], Result>(
  key: QueryKey,
  command: (...args: Args) => Promise<Result>,
  label: string,
  onSuccess?: () => Promise<void>,
) {
  const busy = useIsMutating({ mutationKey: key }) > 0
  const mutation = useMutation({
    mutationKey: key,
    mutationFn: (args: Args) => call(command, ...args),
    meta: { activity: label },
    onSuccess,
  })
  return { run: (...args: Args) => mutation.mutate(args), busy }
}

export function useImportPages() {
  const { run, busy } = useCommand(['import-pages'], commands.import, 'navigator.importing', () =>
    refresh(projectKey, pagesKey, pageKey, chaptersKey),
  )
  return { importPages: run, importing: busy }
}

export async function refresh(...keys: QueryKey[]): Promise<void> {
  await Promise.all(keys.map((queryKey) => queryClient.invalidateQueries({ queryKey })))
}
