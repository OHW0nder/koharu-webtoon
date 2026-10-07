import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { ChapterPane } from '@/components/series/ChapterPane'
import type { SeriesChapter } from '@koharu/bridge/protocol'

const exportChapters = vi.hoisted(() => vi.fn(async () => undefined))

vi.mock('@/lib/backend', () => ({ call: vi.fn(async () => undefined) }))

vi.mock('@/lib/queries', () => ({
  useSeriesDetail: () => ({ data: { chapters } }),
  useExportSeriesChapters: () => ({ exportChapters, exporting: false }),
  useDeleteSeriesChapter: () => ({ deleteChapter: vi.fn(), deletingChapter: false }),
}))

vi.mock('@/lib/store', () => ({
  useJobsRunning: () => false,
  useKoharuStore: (selector: (state: unknown) => unknown) =>
    selector({ showChapter: vi.fn() }),
}))

const chapters = [
  { seq: 10, chapter: 'Ch10', title: 'Chapter 10' },
  { seq: 11, chapter: 'Ch11', title: 'Chapter 11' },
  { seq: 12, chapter: 'Ch12', title: 'Chapter 12' },
] as unknown as SeriesChapter[]

describe('ChapterPane range selection', () => {
  it('exports a typed range without selecting it first', async () => {
    const user = userEvent.setup()
    render(<ChapterPane series='Demo Title' />)

    const exportButton = screen.getByRole('button', { name: 'Export' })
    expect(exportButton, 'neither a pick nor a range yet').toBeDisabled()

    await user.type(screen.getByLabelText('First chapter number'), '10')
    await user.type(screen.getByLabelText('Last chapter number'), '12')

    // The range is enough on its own: no intermediate "apply to selection" click, because a
    // disabled export button after typing a range reads as the range not working at all.
    await waitFor(() => expect(exportButton).toBeEnabled())

    await user.click(exportButton)
    expect(exportChapters).toHaveBeenCalledWith({
      chapters: [
        { series: 'Demo Title', chapter: 'Ch10' },
        { series: 'Demo Title', chapter: 'Ch11' },
        { series: 'Demo Title', chapter: 'Ch12' },
      ],
    })
  })

  it('still exports a hand-picked selection when no range is named', async () => {
    const user = userEvent.setup()
    render(<ChapterPane series='Demo Title' />)

    await user.click(screen.getByRole('checkbox', { name: /Chapter 11/ }))

    const exportButton = screen.getByRole('button', { name: 'Export' })
    await waitFor(() => expect(exportButton).toBeEnabled())
    await user.click(exportButton)

    expect(exportChapters).toHaveBeenCalledWith({
      chapters: [{ series: 'Demo Title', chapter: 'Ch11' }],
    })
  })

  it('adds a named range to the selection so it can be reviewed in the list', async () => {
    const user = userEvent.setup()
    render(<ChapterPane series='Demo Title' />)

    await user.type(screen.getByLabelText('First chapter number'), '11')
    await user.click(screen.getByRole('button', { name: 'Select range' }))

    // The selection now holds the range, so clearing the fields leaves an export that covers the
    // same chapter — proof that "select range" really moved the selection rather than only
    // lighting up the button.
    await user.clear(screen.getByLabelText('First chapter number'))

    const exportButton = screen.getByRole('button', { name: 'Export' })
    await waitFor(() => expect(exportButton).toBeEnabled())
    await user.click(exportButton)
    expect(exportChapters).toHaveBeenCalledWith({
      chapters: [{ series: 'Demo Title', chapter: 'Ch11' }],
    })
  })
})