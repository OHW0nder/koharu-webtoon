import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { AdBandField } from '@/components/series/AdBandField'

/** The stored value is a height, so zero means "no band on this end" rather than a band of zero
 *  pixels. Showing it as text would put a character between the caret and the number the user is
 *  about to type, forcing a trip back to the start of the field to delete it. */
describe('ad band heights', () => {
  const setup = (value: number) => {
    const onChange = vi.fn()
    render(<AdBandField label='head' value={value} clearLabel='clear' onChange={onChange} />)
    return { field: screen.getByLabelText('head'), onChange }
  }

  it('shows an unset height as a placeholder rather than as text', () => {
    const { field } = setup(0)
    expect(field).toHaveValue(null)
    expect(field).toHaveAttribute('placeholder', '0')
  })

  it('lets the first keystroke land directly in the field', () => {
    const { field, onChange } = setup(0)
    // One event, exactly what a click followed by typing produces: "120" and not "0120".
    fireEvent.change(field, { target: { value: '120' } })
    expect(onChange).toHaveBeenCalledWith(120)
  })

  it('reads an emptied field as no band, matching the clear button', () => {
    const { field, onChange } = setup(120)
    fireEvent.change(field, { target: { value: '' } })
    expect(onChange).toHaveBeenCalledWith(0)
  })

  it('rounds a measured height instead of storing a fraction', () => {
    const { field, onChange } = setup(0)
    fireEvent.change(field, { target: { value: '119.6' } })
    expect(onChange).toHaveBeenCalledWith(120)
  })

  it('clamps a negative height to no band', () => {
    const { field, onChange } = setup(0)
    fireEvent.change(field, { target: { value: '-5' } })
    expect(onChange).toHaveBeenCalledWith(0)
  })

  it('keeps clearing the band disabled while there is nothing to clear', () => {
    const { field } = setup(0)
    expect(screen.getByRole('button', { name: 'clear' })).toBeDisabled()
    expect(field).toBeEnabled()
  })
})
