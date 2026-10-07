import { QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

import { SourceUpdatePopover } from '@/components/series/SourceUpdatePopover'
import { queryClient } from '@/lib/queries'
import { useKoharuStore } from '@/lib/store'
import { commands, type SeriesSource, type SourceFetch } from '@koharu/bridge/protocol'

const SOURCE: SeriesSource = { site: 'omega_scans', slug: 'love-quest' }

/** The update popover is the only place raw chapters arrive from a site, so both contracts worth
 *  pinning down are about what crosses the boundary: a pasted address becomes a verified slug, and
 *  the chapters handed back are the ones the check just returned rather than names re-resolved. */
describe('SourceUpdatePopover', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    // `staleTime` is Infinity, so a cache left behind here would feed the next test.
    queryClient.clear()
    useKoharuStore.setState({ sourceFetches: {} })
  })

  it('hands the pasted address to the backend untouched', async () => {
    const bind = vi.spyOn(commands, 'setSeriesSource').mockResolvedValue({
      id: 'Demo Title',
      title: 'Demo Title',
      serial: true,
      cover: null,
      chapters: [],
      settings: { ad: { head: 0, tail: 0 }, guidance: '', glossary: null, context_pages: 4 },
      source: SOURCE,
    })
    renderPopover(null)

    await open()
    fireEvent.change(await screen.findByRole('textbox'), {
      target: { value: 'https://omegascans.org/series/love-quest' },
    })
    fireEvent.click(screen.getByRole('button', { name: /^bind$/i }))

    // Parsing and verifying the slug is the backend's job: a rule duplicated here would be a rule
    // that eventually disagrees with the one that actually writes the index.
    await waitFor(() => expect(bind).toHaveBeenCalledWith('Demo Title', expect.any(String)))
  })

  it('hands the checked chapters back exactly as they arrived', async () => {
    const missing = [
      { name: 'Chapter 36', slug: 'chapter-36' },
      { name: 'Chapter 37', slug: 'chapter-37' },
    ]
    vi.spyOn(commands, 'checkSeriesUpdates').mockResolvedValue({
      title: 'Demo Title',
      kind: 'manga',
      missing,
    })
    const start = vi.spyOn(commands, 'startFetch').mockResolvedValue(1)
    renderPopover(SOURCE)

    await open()
    await check()
    await screen.findByRole('button', { name: /download/i })
    // Everything missing starts picked, so the common case is one click rather than two per chapter.
    // Deselecting one has to narrow what is sent, or the checkbox would be decorative.
    fireEvent.click(screen.getByRole('checkbox', { name: /Chapter 36/ }))
    fireEvent.click(screen.getByRole('button', { name: /download/i }))

    await waitFor(() => expect(start).toHaveBeenCalledWith('Demo Title', [missing[1]]))
  })

  it('says so when the site has nothing this series is missing', async () => {
    vi.spyOn(commands, 'checkSeriesUpdates').mockResolvedValue({
      title: 'Demo Title',
      kind: 'webtoon',
      missing: [],
    })
    renderPopover(SOURCE)

    await open()
    await check()
    expect(await screen.findByText(/already up to date/i)).toBeInTheDocument()
  })

  /** 一次成功的下载曾经被显示成红色失败：终态一栏原来对「非运行中」一律取 `error`，而成功那一趟
   *  的 `error` 是空的，于是落到了兜底的失败文案。用户会以为白下了，于是再下一次。 */
  it('does not call a finished download a failure', async () => {
    useKoharuStore.setState({ sourceFetches: { 'Demo Title': finished(2) } })
    renderPopover(SOURCE)

    await open()
    expect(await screen.findByText(/2 chapters imported/i)).toBeInTheDocument()
    expect(screen.queryByText(/did not finish/i)).toBeNull()
  })

  it('still shows the real error when one arrives', async () => {
    useKoharuStore.setState({
      sourceFetches: {
        'Demo Title': { ...finished(1), state: 'failed', error: '003.jpg could not be fetched' },
      },
    })
    renderPopover(SOURCE)

    await open()
    expect(await screen.findByText('003.jpg could not be fetched')).toBeInTheDocument()
  })
})

function finished(chapterTotal: number) {
  return {
    id: 1,
    series: 'Demo Title',
    chapter: '',
    chapter_index: chapterTotal,
    chapter_total: chapterTotal,
    page_index: 0,
    page_total: 0,
    state: 'finished',
    error: null,
  } satisfies SourceFetch
}

/** The trigger only opens the panel; asking the site is a separate, deliberate click. */
async function open() {
  fireEvent.click(screen.getByRole('button', { name: /fetch updates/i }))
  // Waiting on the panel itself keeps its open transition inside the act boundary, so the update
  // it makes on the way in is not reported as an unwrapped state change.
  await screen.findByRole('dialog')
}

async function check() {
  fireEvent.click(await screen.findByRole('button', { name: /check for updates/i }))
}

function renderPopover(source: SeriesSource | null) {
  return render(
    <QueryClientProvider client={queryClient}>
      <SourceUpdatePopover series='Demo Title' source={source} kind='manga' busy={false} />
    </QueryClientProvider>,
  )
}