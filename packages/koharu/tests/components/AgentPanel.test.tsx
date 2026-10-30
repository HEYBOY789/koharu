import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

import { AgentPanel } from '@/components/editor/AgentPanel'
import { commands, type AgentStatus, type Config, type Event } from '@koharu/bridge/protocol'

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: (event: unknown) => void = () => {}
  },
}))
afterEach(cleanup)
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

it('switches to AGY without a Codex account, streams an edit and locks setup until completion', async () => {
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
  let onEvent: (event: Event) => void = () => {}
  const run = vi.spyOn(commands, 'runAgent').mockImplementation(async (_, channel) => {
    onEvent = channel.onmessage
    onEvent({ type: 'started', run: 'fixture-run' })
    return 'fixture-run'
  })
  render(<AgentPanel />)
  const backend = await screen.findByRole('combobox', { name: 'Agent backend' })
  fireEvent.change(backend, { target: { value: 'agy' } })
  const message = await screen.findByRole('textbox', { name: 'Message Koharu Agent' })
  await waitFor(() => expect(message).toBeEnabled())
  fireEvent.change(message, { target: { value: 'Correct this translation' } })
  fireEvent.click(screen.getByRole('button', { name: 'Send' }))
  await waitFor(() => expect(run).toHaveBeenCalledTimes(1))
  expect(backend).toBeDisabled()
  expect(screen.getByRole('combobox', { name: 'Reasoning' })).toBeDisabled()
  act(() => {
    onEvent({ type: 'tool_started', run: 'fixture-run', call_id: 'edit', name: 'set_translation' })
    onEvent({
      type: 'tool_finished',
      run: 'fixture-run',
      call_id: 'edit',
      name: 'set_translation',
      changed: true,
      output: '{}',
    })
    onEvent({ type: 'text_delta', run: 'fixture-run', delta: 'Corrected' })
    onEvent({ type: 'completed', run: 'fixture-run', message: 'Corrected the translation' })
  })
  expect(screen.getByText('Corrected the translation')).toBeVisible()
  expect(backend).toBeEnabled()
  expect(login).not.toHaveBeenCalled()
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
