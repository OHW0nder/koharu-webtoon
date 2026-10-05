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
  type AdBands,
  type ChapterKind,
  type ExportFormat,
  type FontFamily,
  type Glossary,
  type Operation,
  type SeriesSettings,
} from '@koharu/bridge/protocol'

import { call } from './backend'

export const projectKey = ['project'] as const
export const pagesKey = ['pages'] as const
export const pageKey = ['page'] as const
export const preparedPageKey = (page: string) => ['prepared-page', page] as const
export const fontsKey = ['fonts'] as const
export const seriesKey = ['series'] as const
export const seriesDetailKey = (id: string) => ['series', id] as const
// Deliberately not under `seriesDetailKey`: the chapter list carries the same settings, and
// invalidating it after every settings write would refetch the whole list on each keystroke.
export const seriesSettingsKey = (id: string) => ['series-settings', id] as const
export const seriesGlossaryKey = (id: string) => ['series-glossary', id] as const

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

export function useSeriesSettings(id: string) {
  return useQuery({
    queryKey: seriesSettingsKey(id),
    queryFn: () => call(commands.getSeriesSettings, id),
  })
}

/** The ad bands and the translation guidance, which are one index write and therefore one draft. */
export function useSaveSeriesSettings(id: string) {
  const mutation = useMutation({
    mutationKey: ['set-series-settings', id],
    // `set_series_settings` replaces the whole settings field, so the glossary file name is read
    // back from the cache at write time: a payload carrying a stale `null` would drop the index's
    // reference to a file that is still on disk.
    mutationFn: (draft: Pick<SeriesSettings, 'ad' | 'guidance' | 'context_pages'>) => {
      const current = queryClient.getQueryData<SeriesSettings>(seriesSettingsKey(id))
      return call(commands.setSeriesSettings, id, {
        ad: draft.ad,
        guidance: draft.guidance,
        context_pages: draft.context_pages,
        glossary: current?.glossary ?? null,
      })
    },
    onSuccess: (saved) => {
      queryClient.setQueryData(seriesSettingsKey(id), saved)
      // The chapter list carries the same settings and seeds the import dialog.
      void refresh(seriesDetailKey(id))
    },
  })
  return {
    saveSettings: mutation.mutateAsync,
    savingSettings: useIsMutating({ mutationKey: ['set-series-settings', id] }) > 0,
  }
}

export function useGlossary(id: string) {
  return useQuery({
    queryKey: seriesGlossaryKey(id),
    queryFn: () => call(commands.getGlossary, id),
  })
}

export function useSaveGlossary(id: string) {
  const mutation = useMutation({
    mutationKey: ['set-glossary', id],
    mutationFn: (glossary: Glossary) => call(commands.setGlossary, id, glossary),
    onSuccess: (saved) => {
      queryClient.setQueryData(seriesGlossaryKey(id), saved)
      // Saving also points the index at the file, so the settings the panel reads have to come
      // back before the next settings write reads the glossary name from the cache.
      void refresh(seriesSettingsKey(id), seriesDetailKey(id))
    },
  })
  return {
    saveGlossary: mutation.mutateAsync,
    savingGlossary: useIsMutating({ mutationKey: ['set-glossary', id] }) > 0,
  }
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
    // `ad: null` inherits the series settings; a value applies to this one import and is
    // deliberately not written back to the index. The backend owns that decision either way.
    mutationFn: (input: { name: string; kind: ChapterKind; ad: AdBands | null }) =>
      call(commands.importSeriesChapter, id, input.name, input.kind, input.ad),
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
    // The ad bands are part of the first import because the series does not exist yet, so the
    // index has nowhere to keep them.
    mutationFn: (input: { kind: ChapterKind; ad: AdBands }) =>
      call(commands.importSeries, input.kind, input.ad),
    onSuccess: () => refresh(seriesKey, projectKey, pagesKey, pageKey),
  })
  return {
    importSeries: (input: { kind: ChapterKind; ad: AdBands }) => mutation.mutateAsync(input),
    importing: useIsMutating({ mutationKey: ['import-series'] }) > 0,
  }
}

export async function refresh(...keys: QueryKey[]): Promise<void> {
  await Promise.all(keys.map((queryKey) => queryClient.invalidateQueries({ queryKey })))
}
