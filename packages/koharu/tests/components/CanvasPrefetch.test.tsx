import { act, renderHook, waitFor } from '@testing-library/react'
import { expect, it, vi } from 'vitest'

import { useCanvas } from '@/components/editor/useCanvas'
import { prefetchCanvasPages } from '@koharu/bridge/canvas'

const renderer = vi.hoisted(() => ({
  staged: null as number | null,
  stageManifest: vi.fn((manifest: Uint8Array) => {
    renderer.staged = manifest[0]
    return { token: manifest[0], missing: manifest[0] === 7 ? ['selected-image'] : [] }
  }),
  installResource: vi.fn(async () => undefined),
  hasActiveManifest: vi.fn(() => false),
  activateFrame: vi.fn((token: number) => token === renderer.staged),
  cacheFrame: vi.fn((token: number) => {
    if (token !== renderer.staged) return false
    renderer.staged = null
    return true
  }),
  clear: vi.fn(),
  dispose: vi.fn(),
}))

const native = vi.hoisted(() => ({
  getCanvasManifest: vi.fn(),
  getCanvasResource: vi.fn(),
  prepareCanvasPage: vi.fn(),
  getCanvasPageManifest: vi.fn(),
  getCanvasPageResource: vi.fn(),
}))

vi.mock('@koharu/bridge/protocol', () => ({ commands: native }))
vi.mock('@koharu/bridge/canvas', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@koharu/bridge/canvas')>()),
  createCanvas: vi.fn(async () => renderer),
}))

it('keeps hover prefetch from replacing a selected page still being loaded', async () => {
  let resolve!: (value: number[]) => void
  const selected = new Promise<number[]>((done) => {
    resolve = done
  })
  native.getCanvasManifest.mockResolvedValue([7])
  native.getCanvasResource.mockReturnValue(selected)
  const prepared = { revision: 4, page: { id: 'hovered' } }
  native.prepareCanvasPage.mockResolvedValue(prepared)
  native.getCanvasPageManifest.mockResolvedValue([8])
  const element = document.createElement('canvas')
  const { result } = renderHook(() => useCanvas(element, 4, 7))

  await waitFor(() => expect(native.getCanvasResource).toHaveBeenCalledWith(7, 'selected-image'))
  const prefetched = await prefetchCanvasPages(['hovered'])

  await act(async () => {
    resolve([7])
    await selected
  })
  await waitFor(() => expect(result.current.status).toBe('ready'))
  expect(renderer.activateFrame).toHaveBeenCalledWith(7)
  expect(prefetched).toEqual([])
  expect(native.prepareCanvasPage).not.toHaveBeenCalled()
  expect(await prefetchCanvasPages(['hovered'])).toEqual([prepared])
  expect(renderer.cacheFrame).toHaveBeenCalledWith(8, 'hovered')
})

it('keeps a superseded load from releasing the newer selected frame', async () => {
  let finishOld!: (value: number[]) => void
  let finishNew!: (value: number[]) => void
  const old = new Promise<number[]>((resolve) => {
    finishOld = resolve
  })
  const current = new Promise<number[]>((resolve) => {
    finishNew = resolve
  })
  native.getCanvasManifest.mockImplementation(async (generation: number) => [generation])
  native.getCanvasResource.mockImplementation((generation: number) =>
    generation === 7 ? old : current,
  )
  renderer.stageManifest.mockImplementation((manifest: Uint8Array) => {
    renderer.staged = manifest[0]
    return { token: manifest[0], missing: ['image'] }
  })
  const element = document.createElement('canvas')
  const { result, rerender } = renderHook(({ generation }) => useCanvas(element, 4, generation), {
    initialProps: { generation: 7 },
  })
  await waitFor(() => expect(native.getCanvasResource).toHaveBeenCalledWith(7, 'image'))
  rerender({ generation: 8 })
  await waitFor(() => expect(native.getCanvasResource).toHaveBeenCalledWith(8, 'image'))

  await act(async () => {
    finishOld([7])
    await old
  })
  expect(await prefetchCanvasPages(['hovered'])).toEqual([])
  expect(native.prepareCanvasPage).not.toHaveBeenCalled()
  await act(async () => {
    finishNew([8])
    await current
  })
  await waitFor(() => expect(result.current.status).toBe('ready'))
  expect(result.current.generation).toBe(8)
  expect(renderer.activateFrame).not.toHaveBeenCalledWith(7)
})
