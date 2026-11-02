import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'

import { AgentPanel } from '@/components/editor/AgentPanel'
import { AgentProgress } from '@/components/editor/AgentProgress'
import { receiveAgent, useKoharuStore } from '@/lib/store'
import {
  commands,
  type AgentStatus,
  type Config,
  type AgentSnapshot,
  type AgentRun,
} from '@koharu/bridge/protocol'

const editor = vi.hoisted(() => ({ name: 'A' }))
vi.mock('@/lib/queries', () => ({ useProject: () => ({ data: { name: editor.name } }) }))
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: (event: unknown) => void = () => {}
  },
}))

afterEach(cleanup)
beforeEach(() => {
  editor.name = 'A'
  useKoharuStore.setState({
    agent: { sequence: 0, conversations: [], run: null },
    canvasPage: 'page-3',
    selectedLayers: ['layer-1'],
  })
})
const config: Config = {
  backend: 'codex',
  codex: { model: null, reasoning: 'medium' },
  pi: { binary: null, node: null, reasoning: null, model: null },
  agy: { binary: null, node: null, reasoning: null, model: null },
}
const initial: AgentStatus = {
  account: null,
  models: [],
  config,
  running: null,
  ready: false,
  error: null,
}
const run: AgentRun = {
  id: 'fixture-run',
  project: 'A',
  state: 'running',
  phase: 'starting',
  tool: null,
  page: null,
  element: null,
  stage: null,
  last_page: null,
  changes: 0,
  error: null,
  failures: [],
}

it('keeps failed page details visible while another page succeeds, then removes only resolved entries', () => {
  const failure = {
    id: 'failed-016',
    tool: 'set_typography',
    page: 'Chapter 1 · 16/38 · 016',
    error: 'invalid font weight',
    attempts: 1,
  }
  const current: AgentRun = {
    ...run,
    phase: 'tool',
    tool: 'inspect_page',
    page: 'Chapter 1 · 17/38 · 017',
    last_page: 'Chapter 1 · 17/38 · 017',
    changes: 2,
    failures: [failure],
  }
  const view = render(<AgentProgress run={current} />)
  expect(screen.getByRole('note')).toHaveTextContent('16/38 · 016')
  expect(screen.getByRole('note')).toHaveTextContent('invalid font weight')
  view.rerender(<AgentProgress run={{ ...current, phase: 'thinking', error: null }} />)
  expect(screen.getByRole('note')).toHaveTextContent('16/38 · 016')
  view.rerender(<AgentProgress run={{ ...current, state: 'failed', error: 'Incomplete' }} />)
  expect(screen.getByRole('note')).toHaveTextContent('invalid font weight')
  view.rerender(<AgentProgress run={{ ...current, failures: [] }} />)
  expect(screen.queryByRole('note')).not.toBeInTheDocument()
})

it('distinguishes the current chapter/page from the last target during queries and thinking', () => {
  const page = 'Chapter 12 — A very long chapter title · 3/50 · 003'
  const current: AgentRun = { ...run, phase: 'tool', tool: 'set_typography', page, last_page: page }
  const view = render(<AgentProgress run={current} />)
  expect(screen.getByText(`Target page: ${page}`)).toBeVisible()
  view.rerender(<AgentProgress run={{ ...current, tool: 'list_fonts', page: null }} />)
  expect(screen.queryByText(`Target page: ${page}`)).not.toBeInTheDocument()
  expect(screen.getByText(`Last targeted page: ${page}`)).toBeVisible()
  view.rerender(<AgentProgress run={{ ...current, phase: 'thinking' }} />)
  expect(screen.getByText(`Last targeted page: ${page}`)).toBeVisible()
  view.rerender(<AgentProgress run={{ ...run, project: 'B' }} />)
  expect(screen.queryByText(page, { exact: false })).not.toBeInTheDocument()
})
function snapshot(sequence: number, current: AgentRun = run, text = ''): AgentSnapshot {
  return {
    sequence,
    run: current,
    conversations: [
      {
        project: 'A',
        messages: [
          {
            id: 'user',
            role: 'user',
            text: 'Correct this translation',
            reasoning: '',
            error: false,
            state: null,
          },
          {
            id: current.id,
            role: 'assistant',
            text,
            reasoning: '',
            error: false,
            state: current.state,
          },
        ],
      },
    ],
  }
}

it('switches to AGY without Codex login and reads edits from the native conversation snapshot', async () => {
  let status = initial
  vi.spyOn(commands, 'getAgentStatus').mockImplementation(async () => status)
  const login = vi.spyOn(commands, 'loginAgent')
  vi.spyOn(commands, 'saveAgentConfig').mockImplementation(async (config) => {
    status = {
      ...status,
      config,
      ready: true,
      models: [{ id: 'test-model', name: 'Test model', reasoning: [] }],
    }
    return config
  })
  const send = vi.spyOn(commands, 'runAgent').mockImplementation(async () => {
    receiveAgent(snapshot(1))
    return run.id
  })
  render(<AgentPanel />)
  const backend = await screen.findByRole('combobox', { name: 'Agent backend' })
  fireEvent.change(backend, { target: { value: 'agy' } })
  const message = await screen.findByRole('textbox', { name: 'Message Koharu Agent' })
  await waitFor(() => expect(message).toBeEnabled())
  fireEvent.change(message, { target: { value: 'Correct this translation' } })
  fireEvent.click(screen.getByRole('button', { name: 'Send' }))
  await waitFor(() =>
    expect(send).toHaveBeenCalledWith('Correct this translation', 'A', {
      page: 'page-3',
      elements: ['layer-1'],
    }),
  )
  expect(backend).toBeDisabled()
  act(() =>
    receiveAgent(snapshot(2, { ...run, phase: 'tool', tool: 'set_typography', page: '007' })),
  )
  const bubble = screen.getByRole('status')
  expect(bubble).toHaveTextContent('007')
  act(() => {
    for (let sequence = 3; sequence < 100; sequence++)
      receiveAgent(
        snapshot(sequence, { ...run, phase: 'tool', tool: 'set_typography', page: '007' }),
      )
  })
  expect(screen.getByRole('status')).toBe(bubble)
  act(() =>
    receiveAgent(
      snapshot(100, { ...run, state: 'finished', changes: 1 }, 'Corrected the translation'),
    ),
  )
  expect(screen.getByText('Corrected the translation')).toBeVisible()
  expect(backend).toBeEnabled()
  expect(login).not.toHaveBeenCalled()
})

it('keeps a rejected read visibly running and clears its note after recovery', async () => {
  vi.spyOn(commands, 'getAgentStatus').mockResolvedValue({
    ...initial,
    config: { ...config, backend: 'agy' },
    ready: true,
  })
  render(<AgentPanel />)
  await screen.findByRole('combobox', { name: 'Agent backend' })
  const error = 'inspect_project: unknown field `unknown`; accepted fields are offset and limit'
  act(() => receiveAgent(snapshot(1, { ...run, phase: 'tool', tool: 'inspect_project', error })))
  expect(screen.getByRole('note')).toHaveTextContent(error)
  expect(screen.getByRole('button', { name: 'Stop' })).toBeEnabled()
  act(() =>
    receiveAgent(
      snapshot(2, { ...run, phase: 'tool', tool: 'set_typography', error: null, changes: 1 }),
    ),
  )
  expect(screen.queryByRole('note')).not.toBeInTheDocument()
  act(() =>
    receiveAgent(
      snapshot(
        3,
        { ...run, state: 'finished', error: null, changes: 1 },
        'Recovered and adjusted the text',
      ),
    ),
  )
  expect(screen.getByText('Recovered and adjusted the text')).toBeVisible()
  expect(screen.queryByRole('button', { name: 'Stop' })).not.toBeInTheDocument()
})

it('keeps project A running across unmount and shows a background banner in B without leaking A messages', async () => {
  vi.spyOn(commands, 'getAgentStatus').mockResolvedValue({
    ...initial,
    config: { ...config, backend: 'agy' },
    ready: true,
  })
  const cancel = vi.spyOn(commands, 'cancelAgent').mockResolvedValue(null)
  const view = render(<AgentPanel />)
  await screen.findByRole('combobox', { name: 'Agent backend' })
  act(() => receiveAgent(snapshot(1)))
  view.unmount()
  act(() => receiveAgent(snapshot(2, { ...run, phase: 'tool', tool: 'inspect_page', page: '008' })))
  editor.name = 'B'
  const other = render(<AgentPanel />)
  expect(await screen.findByText('Agent is working on “A”.')).toBeVisible()
  expect(screen.queryByText('Correct this translation')).not.toBeInTheDocument()
  expect(screen.getByRole('textbox', { name: 'Message Koharu Agent' })).toBeDisabled()
  expect(cancel).not.toHaveBeenCalled()
  fireEvent.click(screen.getByRole('button', { name: 'Stop' }))
  expect(cancel).toHaveBeenCalledWith(run.id)
  act(() => receiveAgent(snapshot(3, { ...run, state: 'stopped' })))
  act(() => receiveAgent(snapshot(2, run, 'out of order')))
  expect(useKoharuStore.getState().agent.run?.state).toBe('stopped')
  other.unmount()
  editor.name = 'A'
  render(<AgentPanel />)
  expect(await screen.findByText('Correct this translation')).toBeVisible()
  expect(screen.getAllByText('Correct this translation')).toHaveLength(1)
})

it('keeps CLI errors visible and saves Pi setup without changing AGY settings', async () => {
  const status = {
    ...initial,
    config: { ...config, backend: 'pi' as const },
    error: 'Could not find Pi CLI',
  }
  vi.spyOn(commands, 'getAgentStatus').mockResolvedValue(status)
  const save = vi.spyOn(commands, 'saveAgentConfig').mockImplementation(async (config) => config)
  render(<AgentPanel />)
  expect(await screen.findByRole('alert')).toHaveTextContent('Could not find Pi CLI')
  expect(screen.getByRole('textbox', { name: 'Message Koharu Agent' })).toBeDisabled()
  fireEvent.click(screen.getByText('Advanced setup'))
  fireEvent.change(screen.getByRole('textbox', { name: 'Agent CLI path' }), {
    target: { value: 'C:\\pi\\dist\\cli.js' },
  })
  fireEvent.click(screen.getByRole('button', { name: 'Save and check' }))
  await waitFor(() =>
    expect(save).toHaveBeenCalledWith({
      ...status.config,
      pi: { ...config.pi, binary: 'C:\\pi\\dist\\cli.js' },
    }),
  )
})

it('recovers from status discovery failure instead of remaining on loading', async () => {
  const check = vi
    .spyOn(commands, 'getAgentStatus')
    .mockRejectedValueOnce(new Error('CLI discovery failed'))
    .mockResolvedValue(initial)
  render(<AgentPanel />)
  expect(await screen.findByRole('alert')).toHaveTextContent('CLI discovery failed')
  fireEvent.click(screen.getByRole('button', { name: 'Check connection' }))
  expect(await screen.findByRole('combobox', { name: 'Agent backend' })).toBeEnabled()
  expect(check).toHaveBeenCalledTimes(2)
})
