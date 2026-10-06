import { QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react'
import { act } from 'react'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { StartView } from '@/components/start/StartView'
import { queryClient, seriesDetailKey, seriesKey, useDeleteSeries } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands, type Series, type SeriesSummary } from '@koharu/bridge/protocol'

/** 漫画柜是这一层唯一的入口：项目不再能凭空创建，所以这里没有「新建空项目」可测。 */
describe('StartView', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    // `staleTime` is Infinity, so a cache left behind here would feed the next test.
    queryClient.clear()
    useKoharuStore.setState({ seriesId: null, chapter: null })
  })

  it('lists every series with its progress', async () => {
    vi.spyOn(commands, 'listSeries').mockResolvedValue([
      { id: 'Demo Title', title: 'Demo Title', serial: true, cover: null, chapters: 24, done: 12 },
      {
        id: 'Blue Archive',
        title: 'Blue Archive',
        serial: false,
        cover: null,
        chapters: 1,
        done: 0,
      },
    ])
    renderShelf()

    expect(await screen.findByText('Demo Title')).toBeInTheDocument()
    // 进度是「已完成 / 总数」，所以总数也是用户判断一部漫画缺了多少章的依据。
    expect(screen.getByText(/12\s*\/\s*24/)).toBeInTheDocument()
    expect(screen.getByText('Blue Archive')).toBeInTheDocument()
  })

  it('opens the chapter list of the series that was clicked', async () => {
    vi.spyOn(commands, 'listSeries').mockResolvedValue([
      { id: 'Demo Title', title: 'Demo Title', serial: true, cover: null, chapters: 24, done: 12 },
    ])
    renderShelf()

    const card = (await screen.findByText('Demo Title')).closest('button')
    if (!card) throw new Error('shelf card is not interactive')
    fireEvent.click(card)

    await waitFor(() => expect(useKoharuStore.getState().seriesId).toBe('Demo Title'))
  })
})

const SUMMARY: SeriesSummary = {
  id: 'Demo Title',
  title: 'Demo Title',
  serial: true,
  cover: null,
  chapters: 24,
  done: 12,
}

const DETAIL: Series = {
  ...SUMMARY,
  chapters: [],
  settings: { ad: { head: 0, tail: 0 }, guidance: '', glossary: null, context_pages: 4 },
}

describe('deleting a series', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    queryClient.clear()
  })

  it('leaves no cached copy of a deleted series behind', async () => {
    queryClient.setQueryData(seriesKey, [SUMMARY])
    queryClient.setQueryData(seriesDetailKey(SUMMARY.id), DETAIL)
    vi.spyOn(commands, 'deleteSeries').mockResolvedValue(null)

    const { result } = renderHook(() => useDeleteSeries(), {
      wrapper: ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
      ),
    })
    await act(async () => {
      await result.current.deleteSeries(SUMMARY.id)
    })

    // A surviving entry is a card pointing at a directory that is gone. `staleTime` is
    // Infinity so it never expires on its own, and clicking it hands the backend a dead id.
    expect(queryClient.getQueryData(seriesDetailKey(SUMMARY.id))).toBeUndefined()
  })
})

function renderShelf() {
  return render(
    <QueryClientProvider client={queryClient}>
      <StartView />
    </QueryClientProvider>,
  )
}
