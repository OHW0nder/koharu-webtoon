'use client'

import {
  QueryClient,
  type QueryKey,
  queryOptions,
  useIsMutating,
  useMutation,
  useQuery,
} from '@tanstack/react-query'

import {
  commands,
  type ChapterKind,
  type ExportFormat,
  type FontFamily,
  type Operation,
} from '@koharu/bridge/protocol'

import { call } from './backend'

export const projectKey = ['project'] as const
export const pagesKey = ['pages'] as const
export const pageKey = ['page'] as const
export const preparedPageKey = (page: string) => ['prepared-page', page] as const
export const fontsKey = ['fonts'] as const
export const seriesKey = ['series'] as const
export const seriesDetailKey = (id: string) => ['series', id] as const

const projectQuery = queryOptions({
  queryKey: projectKey,
  queryFn: () => call(commands.getProject),
})

const seriesQuery = queryOptions({
  queryKey: seriesKey,
  queryFn: () => call(commands.listSeries),
})

const seriesDetailQuery = (id: string) =>
  queryOptions({
    queryKey: seriesDetailKey(id),
    queryFn: () => call(commands.getSeries, id),
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

export function usePages(enabled = true) {
  return useQuery({ ...pagesQuery, enabled })
}

export function usePage(enabled = true) {
  return useQuery({ ...pageQuery, enabled })
}

export function useFonts(enabled = true) {
  return useQuery({ ...fontsQuery, enabled })
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
    refresh(projectKey, pagesKey, pageKey),
  )
  return { importPages: run, importing: busy }
}

export function useImportWebtoonPages() {
  const { run, busy } = useCommand(
    ['import-webtoon-pages'],
    commands.importWebtoon,
    'navigator.importing',
    () => refresh(projectKey, pagesKey, pageKey),
  )
  return { importWebtoonPages: run, importing: busy }
}

export function useSeries() {
  return useQuery(seriesQuery)
}

export function useSeriesDetail(id: string) {
  return useQuery(seriesDetailQuery(id))
}

export function useSeriesCandidates(id: string) {
  return useQuery({
    queryKey: [...seriesDetailKey(id), 'candidates'],
    queryFn: () => call(commands.scanSeriesSource, id),
  })
}

export function useImportSeriesChapter(id: string) {
  const mutation = useMutation({
    mutationKey: ['import-series-chapter', id],
    mutationFn: (input: { name: string; kind: ChapterKind }) =>
      call(commands.importSeriesChapter, id, input.name, input.kind),
    onSuccess: () => refresh(seriesDetailKey(id), seriesKey),
  })
  return {
    importChapter: mutation.mutateAsync,
    importingChapter: useIsMutating({ mutationKey: ['import-series-chapter', id] }) > 0,
  }
}

export function useProcessSeriesChapters(id: string) {
  const mutation = useMutation({
    mutationKey: ['process-series-chapters', id],
    mutationFn: (input: { projects: string[]; operation: Operation }) =>
      call(commands.processSeriesChapters, id, input.projects, input.operation),
    onSuccess: () => refresh(seriesDetailKey(id), projectKey, pagesKey, pageKey),
  })
  return {
    processChapters: mutation.mutateAsync,
    processing: useIsMutating({ mutationKey: ['process-series-chapters', id] }) > 0,
  }
}

export function useExportSeriesChapters(id: string) {
  const mutation = useMutation({
    mutationKey: ['export-series-chapters', id],
    mutationFn: (input: { projects: string[]; format: ExportFormat }) =>
      call(commands.exportSeriesChapters, id, input.projects, input.format),
  })
  return {
    exportChapters: mutation.mutateAsync,
    exporting: useIsMutating({ mutationKey: ['export-series-chapters', id] }) > 0,
  }
}

export function useImportSeries() {
  // `mutateAsync` rather than the shared `useCommand` helper: the caller needs the created series
  // so it can open the new chapter list instead of making the user find it again.
  const mutation = useMutation({
    mutationKey: ['import-series'],
    mutationFn: (kind: ChapterKind) => call(commands.importSeries, kind),
    onSuccess: () => refresh(seriesKey, projectKey, pagesKey, pageKey),
  })
  return {
    importSeries: (kind: ChapterKind) => mutation.mutateAsync(kind),
    importing: useIsMutating({ mutationKey: ['import-series'] }) > 0,
  }
}

export async function refresh(...keys: QueryKey[]): Promise<void> {
  await Promise.all(keys.map((queryKey) => queryClient.invalidateQueries({ queryKey })))
}
