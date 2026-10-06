import { QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { StartView } from '@/components/start/StartView'
import { queryClient } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands } from '@koharu/bridge/protocol'

/** 漫画柜是这一层唯一的入口：项目不再能凭空创建，所以这里没有「新建空项目」可测。 */
describe('StartView', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    useKoharuStore.setState({ seriesId: null })
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

function renderShelf() {
  return render(
    <QueryClientProvider client={queryClient}>
      <StartView />
    </QueryClientProvider>,
  )
}
